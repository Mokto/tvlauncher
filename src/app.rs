//! The launcher's state: rows of cards, focus, the options menu, and one frame of drawing.
use crate::data::{self, apply_debloat, boot_on, set_boot, Item, Kind, Luna, Prefs, Row, DEBLOAT};
use crate::gfx::*;
use crate::iptv::{self, Channel, Got, Guide};
use crate::jellyfin::{self, Watch, DEFAULT_APP};
use crate::keyboard::{Prompt, Typed};
use crate::login::{Login, Signed};
use crate::plex::{self, Tile as PlexTile};
use crate::sdl::*;
use crate::settings::{Act, Category, Entry, Out, Page, Value};
use crate::tv;
use crate::youtube::{self, Tile};
use std::sync::mpsc::{Receiver, Sender};

/// How much of the background picture the rows cover: the rest shows through, as on Android TV.
const SHELF: f32 = 0.84;
/// How long OK is held on an app before its options open.
const HOLD_MS: u32 = 700;
/// How long a featured movie or series stays on Home before the next one.
const FEATURED_MS: u32 = 30_000;
const PINNED: usize = 0;
const WATCH: usize = 1;
const PLEX: usize = 2;
const YOUTUBE: usize = 3;
const APPS: usize = 4;
const INPUTS: usize = 5;

/// The size of a row's cards, as on Android TV: pictures to watch are large, apps and inputs small.
fn card_size(row: usize) -> (f32, f32) {
    match row {
        WATCH | PLEX | YOUTUBE => (384.0, 216.0),
        _ => (256.0, 144.0),
    }
}

/// The space a row takes: its title, its cards and the labels under them.
fn row_height(row: usize) -> f32 {
    LABEL_H + card_size(row).1 + 76.0
}

enum Opt {
    Move,
    Hide,
    Show,
    Pin,
    Unpin,
    /// Ask for confirmation before uninstalling the focused app.
    AskUninstall,
    Uninstall,
    RefreshJellyfin,
    SignOut,
    RefreshYoutube,
    YoutubeSignOut,
    RefreshPlex,
    PlexSignOut,
    /// Open Plex items in the app with this id.
    UsePlexApp(String),
    /// Open Jellyfin items in the app with this id.
    UseApp(String),
    /// Ask for the address of an M3U playlist, or for an Xtream Codes login.
    AskM3u,
    AskXtream,
    RefreshTv,
    RemoveTv,
    /// Add or remove the focused channel (its key) from Favourites.
    Fav(String),
    Unfav(String),
    /// The details of the focused programme.
    TvDetails,
    Close,
}

/// What a text prompt is asking for; the Xtream login takes three in a row.
enum Ask {
    M3u,
    XtServer,
    XtUser(String),
    XtPass(String, String),
    Guide,
}

struct Menu {
    title: Option<Tex>,
    opts: Vec<(Opt, Option<Tex>)>,
    sel: usize,
}

enum Mode {
    Browse,
    Menu(Menu),
    /// Moving an app within its row; holds the order from before, for Back.
    Moving(Vec<String>),
    Login(Box<Login>),
    Settings(Box<Page>),
    Prompt(Box<Prompt>, Ask),
}

pub struct Background {
    pub req: Sender<(u64, Option<String>)>,
    pub res: Receiver<(u64, Option<Surf>)>,
}

pub struct Jelly {
    pub cfg: jellyfin::Config,
    pub refresh: Sender<()>,
    pub res: Receiver<Option<Vec<Watch>>>,
}

/// The signed-in Plex account and its refresh thread.
struct Px {
    cfg: plex::Config,
    refresh: Sender<()>,
    res: Receiver<Option<Vec<PlexTile>>>,
}

/// The live TV source, its channels and guide, the TV tab and the threads that refresh them and fetch logos.
struct Tv {
    cfg: iptv::Config,
    refresh: Sender<bool>,
    res: Receiver<Got>,
    list: Vec<Channel>,
    guide: Guide,
    page: tv::Page,
    logo_req: Sender<Vec<String>>,
    logo_res: Receiver<String>,
}

/// The signed-in YouTube account and its refresh thread.
struct Yt {
    cfg: youtube::Config,
    refresh: Sender<bool>,
    res: Receiver<Option<Vec<Tile>>>,
}

pub struct App {
    g: Gfx,
    luna: Luna,
    win: *mut Opaque,
    prefs: Prefs,
    rows: Vec<Row>,
    /// Hidden apps while they are not shown.
    pool: Vec<Item>,
    show_hidden: bool,
    mode: Mode,
    cur_row: usize,
    /// False until the first key or pointer move: a list that arrives before then takes the focus.
    touched: bool,
    pub animating: bool,
    row_scroll: f32,
    hero_a: f32,
    hero_key: Option<String>,
    glow: [f32; 3],
    /// The background image and the one fading out behind it.
    bg: [Option<Tex>; 2],
    bg_a: [f32; 2],
    bg_path: Option<String>,
    /// The picture behind Home while the focused card has none of its own.
    home_bg: Option<jellyfin::Backdrop>,
    /// When the featured backdrop last changed (SDL ticks).
    home_bg_at: u32,
    /// The hero text of the background's movie or series, made once per picture: (path, title, description).
    home_hero: Option<(String, Option<Tex>, Option<Tex>)>,
    bg_gen: u64,
    bg_pending: bool,
    jf_pending: bool,
    yt_pending: bool,
    px_pending: bool,
    /// Set while an app is being uninstalled; the app list is read again when it ends.
    rm_rx: Option<Receiver<()>>,
    bgl: Background,
    jf: Option<Jelly>,
    yt: Option<Yt>,
    px: Option<Px>,
    tv: Option<Tv>,
    tv_pending: bool,
    /// The TV tab is shown instead of Home (only while live TV is set up).
    on_tv: bool,
    /// The focus is on the Home and TV tabs at the top.
    tabs_f: bool,
    tab_tex: [Option<Tex>; 2],
    /// Made when the first channel is played, and kept (see `player::Player`).
    player: Option<crate::player::Player>,
    /// Why the last channel could not start, shown on the TV tab.
    tv_note: Option<Tex>,
    clock: Option<Tex>,
    clock_min: i32,
    loaded_at: u32,
    hint_options: Option<Tex>,
    hint_move: Option<Tex>,
    wake: u32,
    /// A list that arrives after signing in takes the focus even though a key was pressed.
    just_signed_in: bool,
    /// The cog at the top right has the focus instead of a card.
    cog: bool,
    /// Where the focus was on the settings page, so it is still there after a dialog or a change.
    last_settings: (usize, usize, bool),
    /// The settings page is open, or a dialog that was opened from it: closing the dialog returns to it.
    in_settings: bool,
    /// OK is held on an app: since when. A tap launches it when OK is released, a hold opens its options.
    ok_down: Option<u32>,
    cog_f: f32,
}

impl App {
    pub fn new(g: Gfx, luna: Luna, win: *mut Opaque, bgl: Background, jf: Option<Jelly>, wake: u32) -> App {
        let prefs = Prefs::load(&luna.root);
        let rows = vec![
            Row::new("Pinned apps"),
            Row::new("Continue watching"),
            Row::new("Continue watching on Plex"),
            Row::new("Recommended on YouTube"),
            Row::new("Apps"),
            Row::new("Inputs"),
        ];
        let mut app = App {
            g,
            luna,
            win,
            prefs,
            rows,
            pool: vec![],
            show_hidden: false,
            mode: Mode::Browse,
            cur_row: 0,
            touched: false,
            animating: false,
            row_scroll: 0.0,
            hero_a: 0.0,
            hero_key: None,
            glow: [0.0; 3],
            bg: [None, None],
            bg_a: [0.0; 2],
            bg_path: None,
            home_bg: None,
            home_bg_at: 0,
            home_hero: None,
            bg_gen: 0,
            bg_pending: false,
            jf_pending: false,
            yt_pending: false,
            px_pending: false,
            rm_rx: None,
            bgl,
            jf,
            yt: None,
            px: None,
            tv: None,
            tv_pending: false,
            on_tv: false,
            tabs_f: false,
            tab_tex: [None, None],
            player: None,
            tv_note: None,
            clock: None,
            clock_min: -1,
            loaded_at: 0,
            hint_options: None,
            hint_move: None,
            wake,
            just_signed_in: false,
            cog: false,
            last_settings: (0, 0, false),
            in_settings: false,
            ok_down: None,
            cog_f: 0.0,
        };
        if let Some(jf) = &app.jf {
            let jf_app = app.jf_app();
            let cached: Vec<Item> = jellyfin::cached(&app.luna.root)
                .iter()
                .map(|w| jf.cfg.item(w, &jf_app))
                .collect();
            app.rows[WATCH].items = cached;
            app.jf_pending = jf.refresh.send(()).is_ok();
        }
        if let Some(cfg) = youtube::Config::load(&app.luna.root) {
            app.rows[YOUTUBE].items = youtube::cached(&app.luna.root).iter().map(Tile::item).collect();
            app.start_youtube(cfg, false);
        }
        if let Some(cfg) = plex::Config::load(&app.luna.root) {
            let plex_app = app.plex_app();
            app.rows[PLEX].items = plex::cached(&app.luna.root).iter().map(|t| t.item(&plex_app)).collect();
            app.start_plex(cfg);
        }
        if let Some(cfg) = iptv::Config::load(&app.luna.root) {
            app.start_tv(cfg, false);
        }
        app.load();
        app.pick_home_bg(true);
        app
    }

    /// Shows the cached channels and guide at once, and asks for fresh ones (`force`, or when they are old).
    fn start_tv(&mut self, cfg: iptv::Config, force: bool) {
        let root = self.luna.root.clone();
        let (refresh, res) = iptv::spawn(root.clone(), cfg.clone(), self.wake);
        let (logo_req, logo_res) = iptv::logos(root.clone(), self.wake);
        let list = iptv::cached_channels(&root);
        let page = tv::Page::new(&self.g, &list, &self.prefs.tv_favs);
        let _ = logo_req.send(page.logos(&list));
        self.tv_pending = refresh.send(force).is_ok();
        self.tv = Some(Tv {
            cfg,
            refresh,
            res,
            list,
            guide: iptv::cached_guide(&root),
            page,
            logo_req,
            logo_res,
        });
    }

    fn refresh_tv(&mut self, force: bool) {
        if let Some(tv) = &self.tv {
            self.tv_pending = tv.refresh.send(force).is_ok();
        }
    }

    fn remove_tv(&mut self) {
        self.tv = None;
        self.tv_pending = false;
        self.on_tv = false;
        self.tabs_f = false;
        iptv::sign_out(&self.luna.root);
    }

    /// Builds the TV tab again (new channels or favourites), keeping its focus.
    fn rebuild_tv(&mut self) {
        let Some(tv) = &mut self.tv else { return };
        let mut page = tv::Page::new(&self.g, &tv.list, &self.prefs.tv_favs);
        page.keep(&tv.page, &tv.list, &tv.guide);
        tv.page = page;
        let _ = tv.logo_req.send(tv.page.logos(&tv.list));
    }

    /// Saves what the TV settings asked for and loads the channels from it.
    fn set_tv(&mut self, source: Option<iptv::Source>, guide: Option<Option<String>>) {
        let old = self.tv.as_ref().map(|t| t.cfg.clone());
        let Some(source) = source.or_else(|| old.as_ref().map(|c| c.source.clone())) else {
            return;
        };
        let cfg = iptv::Config {
            source,
            guide: guide.unwrap_or_else(|| old.and_then(|c| c.guide)),
        };
        if let Err(e) = cfg.save(&self.luna.root) {
            log(&format!("iptv.json: {e}"));
        }
        iptv::sign_out_cache(&self.luna.root);
        self.start_tv(cfg, true);
    }

    /// Plays a channel full screen; back on the TV tab, the focus is on the channel it ended on.
    fn watch(&mut self, at: usize) {
        if !self.luna.root.is_empty() {
            return log("play: the simulator has no player");
        }
        let Some(tv) = self.tv.as_mut().filter(|t| at < t.list.len()) else {
            return;
        };
        if self.player.is_none() {
            match crate::player::Player::new() {
                Ok(p) => self.player = Some(p),
                Err(e) => {
                    log(&format!("play: {e}"));
                    self.tv_note = self
                        .g
                        .text(self.g.f_sub, &format!("The player could not start: {e}"), 1100);
                    return;
                }
            }
        }
        if let Some(p) = &mut self.player {
            let end = p.watch(&self.g, &tv.list, at, &tv.guide);
            tv.page.focus(end, &tv.list, &tv.guide);
        }
    }

    /// Chooses the picture behind the apps from the saved set: another one than now when `fresh`, else only
    /// when there is none yet or the one shown is gone.
    fn pick_home_bg(&mut self, fresh: bool) {
        let pool = jellyfin::backdrops(&self.luna.root);
        let current = self.home_bg.as_ref().map(|b| b.path.clone());
        let keep = current.as_ref().is_some_and(|c| pool.iter().any(|b| b.path == *c));
        if pool.is_empty() || (keep && !fresh) {
            self.home_bg = self.home_bg.take().filter(|b| pool.iter().any(|p| p.path == b.path));
            return;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as usize);
        let others: Vec<&jellyfin::Backdrop> = pool.iter().filter(|p| Some(&p.path) != current.as_ref()).collect();
        let from = if others.is_empty() {
            pool.iter().collect()
        } else {
            others
        };
        self.home_bg = Some(from[nanos % from.len()].clone());
        self.home_bg_at = unsafe { SDL_GetTicks() };
    }

    /// Home shows a featured movie or series (its backdrop, title and description) while the focused card has
    /// no picture of its own, as Android TV does.
    fn featured(&self, focus: Option<(usize, usize)>) -> Option<&jellyfin::Backdrop> {
        let own = focus.is_some_and(|(r, c)| self.rows[r].items[c].bg.is_some());
        self.home_bg.as_ref().filter(|b| !own && !b.title.is_empty())
    }

    pub fn busy(&self) -> bool {
        self.bg_pending
            || self.jf_pending
            || self.yt_pending
            || self.px_pending
            || self.tv_pending
            || self.rm_rx.is_some()
            || matches!(&self.mode, Mode::Login(l) if l.busy())
    }

    fn jf_app(&self) -> String {
        self.prefs.jf_app.clone().unwrap_or_else(|| DEFAULT_APP.into())
    }

    /// The app that opens Plex items: the one chosen in Settings, else PlxNative, else any Plex app that is installed.
    fn plex_app(&self) -> String {
        if let Some(id) = &self.prefs.plex_app {
            return id.clone();
        }
        self.all_apps()
            .map(|i| i.id.as_str())
            .find(|id| *id == plex::DEFAULT_APP)
            .or_else(|| self.all_apps().map(|i| i.id.as_str()).find(|id| id.contains("plex")))
            .unwrap_or(plex::DEFAULT_APP)
            .into()
    }

    /// Every installed app, hidden or not.
    fn all_apps(&self) -> impl Iterator<Item = &Item> {
        self.rows[APPS]
            .items
            .iter()
            .chain(&self.rows[PINNED].items)
            .chain(&self.pool)
            .filter(|i| i.kind == Kind::App)
    }

    fn settings_cats(&self) -> Vec<Category> {
        let text = |t: &str| Value::Text(t.to_string());
        let entry = |act, title: &str, desc: &str, value| Entry {
            act,
            title: title.into(),
            desc: desc.into(),
            value,
        };
        let id = self.jf_app();
        let app = self
            .all_apps()
            .find(|i| i.id == id)
            .map_or_else(|| format!("{id} (not installed)"), |i| i.title.clone());
        let jellyfin = vec![
            entry(
                Act::Jellyfin,
                "Account",
                "Where Continue watching and the background pictures come from.",
                match &self.jf {
                    Some(jf) => text(&jf.cfg.user),
                    None => text("Sign in"),
                },
            ),
            entry(
                Act::JfApp,
                "Open videos in",
                "The app that opens what you pick in Continue watching.",
                text(&app),
            ),
        ];
        let youtube = vec![entry(
            Act::Youtube,
            "Account",
            "Recommended videos from your YouTube account. Unofficial: it uses the sign-in of YouTube's TV app.",
            text(if self.yt.is_some() { "Signed in" } else { "Sign in" }),
        )];
        let plex_app = self.plex_app();
        let plex_title = self
            .all_apps()
            .find(|i| i.id == plex_app)
            .map_or_else(|| format!("{plex_app} (not installed)"), |i| i.title.clone());
        let plex = vec![
            entry(
                Act::Plex,
                "Account",
                "Where Continue watching on Plex comes from. Sign in with a code you enter at plex.tv/link.",
                match &self.px {
                    Some(px) => text(&px.cfg.user),
                    None => text("Sign in"),
                },
            ),
            entry(
                Act::PlexApp,
                "Open videos in",
                "The app that opens what you pick. PlxNative opens the title itself; other apps only start.",
                text(&plex_title),
            ),
        ];
        let host = |url: &str| {
            let rest = url.split_once("://").map_or(url, |(_, r)| r);
            rest.split(['/', '?']).next().unwrap_or(rest).to_string()
        };
        let tv_cfg = self.tv.as_ref().map(|t| &t.cfg);
        let tv = vec![
            entry(
                Act::TvSource,
                "Channels",
                "Your own M3U playlist or Xtream Codes login. They appear in a TV tab.",
                text(&match tv_cfg.map(|c| &c.source) {
                    Some(iptv::Source::M3u(url)) => jellyfin::clip(&host(url), 18),
                    Some(iptv::Source::Xtream { server, .. }) => jellyfin::clip(&host(server), 18),
                    None => "Set up".into(),
                }),
            ),
            entry(
                Act::TvGuide,
                "Guide",
                "An XMLTV address for what is on now and next, if the channels bring none.",
                text(&match tv_cfg.and_then(|c| c.guide.as_deref()) {
                    Some(url) => jellyfin::clip(&host(url), 18),
                    None => "Automatic".into(),
                }),
            ),
        ];
        let describe = |key: &str| match key {
            "ads" => "Stops the ads on LG's home screen and the servers that deliver them.",
            "tracking" => "Stops the TV from reporting what is on screen.",
            "logs" => "Stops LG's diagnostic and analytics logging.",
            _ => "Stops LG's recommendations and promotions.",
        };
        let privacy = DEBLOAT
            .iter()
            .map(|(key, title)| {
                let on = self.prefs.debloat.iter().any(|g| g == key);
                entry(Act::Debloat(key), title, describe(key), Value::Switch(on))
            })
            .collect();
        let startup = vec![entry(
            Act::Boot,
            "Start as Home",
            "Opens this launcher as the Home screen every time the TV starts. If it keeps crashing, LG's Home comes back.",
            Value::Switch(boot_on(&self.luna.root)),
        )];
        [
            ("Jellyfin", jellyfin),
            ("Plex", plex),
            ("YouTube", youtube),
            ("Live TV", tv),
            ("Ads and privacy", privacy),
            ("Startup", startup),
        ]
        .into_iter()
        .map(|(name, entries)| Category {
            name: name.into(),
            entries,
        })
        .collect()
    }

    /// Opens the settings page, or builds it again after a change, with the focus where it was.
    fn show_settings(&mut self) {
        let (cat, row, pane) = match &self.mode {
            Mode::Settings(p) => (p.cat, p.row, p.pane),
            _ => self.last_settings,
        };
        let page = Page::new(&self.g, self.settings_cats(), cat, row, pane);
        self.in_settings = true;
        self.mode = Mode::Settings(Box::new(page));
    }

    fn open_settings(&mut self) {
        self.last_settings = (0, 0, false);
        self.show_settings();
    }

    /// A dialog is done: back to the settings page it was opened from, else to Home.
    fn close_dialog(&mut self) {
        if self.in_settings {
            self.show_settings();
        } else {
            self.mode = Mode::Browse;
        }
    }

    fn signed_in(&mut self, signed: Signed) {
        self.just_signed_in = true;
        match signed {
            Signed::Jellyfin(cfg) => {
                self.prefs.jf_server = Some(cfg.server.clone());
                self.prefs.save();
                let (refresh, res) = jellyfin::spawn(self.luna.root.clone(), cfg.clone(), self.wake);
                self.jf_pending = refresh.send(()).is_ok();
                self.jf = Some(Jelly { cfg, refresh, res });
            }
            Signed::Youtube => {
                if let Some(cfg) = youtube::Config::load(&self.luna.root) {
                    self.start_youtube(cfg, true);
                }
            }
            Signed::Plex => {
                if let Some(cfg) = plex::Config::load(&self.luna.root) {
                    self.start_plex(cfg);
                }
            }
        }
        self.close_dialog();
    }

    fn start_plex(&mut self, cfg: plex::Config) {
        let (refresh, res) = plex::spawn(self.luna.root.clone(), cfg.clone(), self.wake);
        self.px_pending = refresh.send(()).is_ok();
        self.px = Some(Px { cfg, refresh, res });
    }

    fn sign_out_plex(&mut self) {
        self.px = None;
        plex::sign_out(&self.luna.root);
        self.px_pending = false;
        self.rows[PLEX].items.clear();
        self.refocus(None, self.rows[self.cur_row].col);
    }

    fn refresh_plex(&mut self) {
        if let Some(px) = &self.px {
            self.px_pending = px.refresh.send(()).is_ok();
        }
    }

    fn start_youtube(&mut self, cfg: youtube::Config, force: bool) {
        let (refresh, res) = youtube::spawn(self.luna.root.clone(), cfg.clone(), self.wake);
        self.yt_pending = refresh.send(force).is_ok();
        self.yt = Some(Yt { cfg, refresh, res });
    }

    fn sign_out_youtube(&mut self) {
        if let Some(yt) = self.yt.take() {
            youtube::sign_out(&self.luna.root, &yt.cfg);
        }
        self.yt_pending = false;
        self.rows[YOUTUBE].items.clear();
        self.refocus(None, self.rows[self.cur_row].col);
    }

    /// Asks for the feed now, or unless the saved one is young (`force` false).
    fn refresh_youtube(&mut self, force: bool) {
        if let Some(yt) = &self.yt {
            self.yt_pending = yt.refresh.send(force).is_ok();
        }
    }

    fn sign_out(&mut self) {
        if let Some(jf) = self.jf.take() {
            jellyfin::sign_out(&self.luna.root, &jf.cfg);
        }
        self.jf_pending = false;
        self.rows[WATCH].items.clear();
        self.refocus(None, self.rows[self.cur_row].col);
    }

    fn refresh_jellyfin(&mut self) {
        if let Some(jf) = &self.jf {
            self.jf_pending = jf.refresh.send(()).is_ok();
        }
    }

    fn focused(&self) -> Option<(usize, usize)> {
        let r = &self.rows[self.cur_row];
        (!r.items.is_empty() && !self.top() && !self.on_tv).then_some((self.cur_row, r.col))
    }

    fn focused_key(&self) -> Option<String> {
        self.focused().map(|(r, c)| self.rows[r].items[c].key.clone())
    }

    fn first_row(&self) -> usize {
        self.rows.iter().position(|r| !r.items.is_empty()).unwrap_or(0)
    }

    /// Position of row `r` among the rows that have cards.
    fn vpos(&self, r: usize) -> usize {
        self.rows[..r].iter().filter(|r| !r.items.is_empty()).count()
    }

    /// Puts the focus back on `key` after the rows changed, else keeps the column where it was.
    fn refocus(&mut self, key: Option<String>, col: usize) {
        if let Some(k) = key {
            for (r, row) in self.rows.iter_mut().enumerate() {
                if let Some(i) = row.items.iter().position(|it| it.key == k) {
                    self.cur_row = r;
                    row.col = i;
                    return;
                }
            }
        }
        if self.rows[self.cur_row].items.is_empty() {
            self.cur_row = self.first_row();
        }
        let row = &mut self.rows[self.cur_row];
        row.col = col.min(row.items.len().saturating_sub(1));
    }

    /// Start settled on the focus instead of sliding to it.
    fn settle(&mut self) {
        self.row_scroll = self.vpos(self.cur_row) as f32;
        for (r, row) in self.rows.iter_mut().enumerate() {
            row.scroll = row.col as f32 * (card_size(r).0 + GAP);
            row.active = f32::from(r == self.cur_row);
            if let Some(it) = row.items.get_mut(row.col) {
                it.focus = f32::from(r == self.cur_row);
            }
        }
    }

    fn load(&mut self) {
        self.rows[APPS].items = data::apps(&self.luna);
        self.rows[PINNED].items.clear();
        self.pool.clear();
        self.rebuild_apps();
        self.rows[INPUTS].items = data::inputs(&self.luna);
        self.hero_key = None;
        self.refocus(None, self.rows[self.cur_row].col);
        for r in &mut self.rows {
            r.col = r.col.min(r.items.len().saturating_sub(1));
        }
        self.cur_row = if self.rows[self.cur_row].items.is_empty() {
            self.first_row()
        } else {
            self.cur_row
        };
        if self.luna.root.is_empty() {
            if let Ok(last) = std::fs::read_to_string(data::FOCUS_FILE) {
                self.refocus(Some(last), 0);
            }
        }
        self.settle();
        log(&format!(
            "{} watching, {} apps, {} inputs",
            self.rows[WATCH].items.len(),
            self.rows[APPS].items.len() + self.rows[PINNED].items.len() + self.pool.len(),
            self.rows[INPUTS].items.len()
        ));
    }

    /// Sorts the apps by the saved order, parks the hidden ones in the pool (or shows them dimmed) and adds the
    /// toggle card.
    fn rebuild_apps(&mut self) {
        let mut all = std::mem::take(&mut self.rows[APPS].items);
        all.append(&mut self.rows[PINNED].items);
        all.append(&mut self.pool);
        all.retain(|i| i.kind != Kind::Toggle);
        let prefs = &self.prefs;
        all.sort_by_key(|i| {
            (
                prefs.order.iter().position(|o| *o == i.key).unwrap_or(usize::MAX),
                i.tv_index,
            )
        });
        for i in &mut all {
            i.hidden = prefs.hidden.contains(&i.key);
        }
        let (mut pinned, all): (Vec<Item>, Vec<Item>) = all
            .into_iter()
            .partition(|i| !i.hidden && prefs.pinned.contains(&i.key));
        pinned.sort_by_key(|i| prefs.pinned.iter().position(|k| *k == i.key));
        self.rows[PINNED].items = pinned;
        let hidden = all.iter().filter(|i| i.hidden).count();
        if hidden == 0 {
            self.show_hidden = false;
        }
        let (mut shown, pool): (Vec<Item>, Vec<Item>) = all.into_iter().partition(|i| !i.hidden || self.show_hidden);
        self.pool = pool;
        if hidden > 0 {
            let title = if self.show_hidden {
                "Hide hidden apps".to_string()
            } else {
                format!("Hidden apps ({hidden})")
            };
            let mut t = Item::new("toggle".into(), "toggle".into(), Kind::Toggle, Some(title));
            t.color = rgb(30, 33, 37);
            t.sub = Some(if self.show_hidden {
                "Hidden apps are shown dimmed. Press Options on one to put it back on Home.".into()
            } else {
                "Show the apps you hid from Home.".into()
            });
            shown.push(t);
        }
        self.rows[APPS].items = shown;
    }

    fn apply_watching(&mut self, list: Vec<Watch>) {
        log(&format!("jellyfin: {} items", list.len()));
        let Some(jf) = &self.jf else { return };
        let jf_app = self.jf_app();
        let items = list.iter().map(|w| jf.cfg.item(w, &jf_app)).collect();
        self.set_row(WATCH, items);
    }

    fn apply_plex(&mut self, list: Vec<PlexTile>) {
        log(&format!("plex: {} items", list.len()));
        let app = self.plex_app();
        self.set_row(PLEX, list.iter().map(|t| t.item(&app)).collect());
    }

    fn apply_youtube(&mut self, list: Vec<Tile>) {
        log(&format!("youtube: {} items", list.len()));
        self.set_row(YOUTUBE, list.iter().map(Tile::item).collect());
    }

    /// Replaces a row of videos, keeping what is already drawn for the cards that stay. The first list takes the
    /// focus unless a key was pressed before it arrived, or the user just signed in.
    fn set_row(&mut self, r: usize, mut items: Vec<Item>) {
        let (key, col) = (self.focused_key(), self.rows[self.cur_row].col);
        let mut old = std::mem::take(&mut self.rows[r].items);
        for it in &mut items {
            if let Some(o) = old.iter_mut().find(|o| o.key == it.key) {
                it.focus = o.focus;
                if o.icons == it.icons {
                    (it.icon, it.icon_tried) = (o.icon.take(), o.icon_tried);
                }
                if o.title == it.title {
                    (it.label, it.hero) = (o.label.take(), o.hero.take());
                }
                if o.sub == it.sub {
                    it.hero_sub = o.hero_sub.take();
                }
            }
        }
        let first = old.is_empty() && !items.is_empty();
        let nothing_above = self.rows[..r].iter().all(|row| row.items.is_empty());
        self.rows[r].items = items;
        if first && nothing_above && (!self.touched || self.just_signed_in) {
            self.just_signed_in = false;
            self.cur_row = r;
            self.rows[r].col = 0;
        } else {
            self.refocus(key, col);
        }
    }

    fn request_bg(&mut self, p: Option<&str>) {
        if p == self.bg_path.as_deref() {
            return;
        }
        self.bg_path = p.map(String::from);
        self.bg_gen += 1;
        self.bg_pending = true;
        let _ = self.bgl.req.send((self.bg_gen, p.map(|p| self.g.rp(p))));
    }

    fn take_bg(&mut self) {
        while let Ok((gen, surf)) = self.bgl.res.try_recv() {
            if gen != self.bg_gen {
                continue;
            }
            self.bg_pending = false;
            self.bg[1] = self.bg[0].take();
            self.bg_a[1] = self.bg_a[0];
            self.bg[0] = surf.and_then(|s| Tex::new(self.g.ren, &s));
            self.bg_a[0] = 0.0;
        }
    }

    pub fn frame(&mut self, dt: f32) {
        if let Mode::Login(l) = &mut self.mode {
            if let Some(signed) = l.poll(&self.g) {
                self.signed_in(signed);
            }
        }
        while let Some(list) = self.jf.as_ref().and_then(|j| j.res.try_recv().ok()) {
            self.jf_pending = false;
            match list {
                Some(list) => self.apply_watching(list),
                None => self.pick_home_bg(false),
            }
        }
        if self.rm_rx.as_ref().is_some_and(|rx| rx.try_recv().is_ok()) {
            self.rm_rx = None;
            self.load();
        }
        while let Some(list) = self.px.as_ref().and_then(|p| p.res.try_recv().ok()) {
            self.px_pending = false;
            if let Some(list) = list {
                self.apply_plex(list);
            }
        }
        while let Some(list) = self.yt.as_ref().and_then(|y| y.res.try_recv().ok()) {
            self.yt_pending = false;
            if let Some(list) = list {
                self.apply_youtube(list);
            }
        }
        while let Some(got) = self.tv.as_ref().and_then(|t| t.res.try_recv().ok()) {
            match got {
                Got::Channels(list) => {
                    if let Some(tv) = &mut self.tv {
                        tv.list = list;
                    }
                    self.tv_note = None;
                    self.rebuild_tv();
                }
                Got::Guide(guide) => {
                    if let Some(tv) = &mut self.tv {
                        tv.guide = guide;
                        tv.page.forget();
                    }
                }
                Got::Failed(e) => {
                    self.tv_note = self
                        .g
                        .text(self.g.f_sub, &format!("The channels could not be loaded: {e}"), 1100);
                }
                Got::Done => self.tv_pending = false,
            }
        }
        while let Some(tv) = &mut self.tv {
            let Ok(url) = tv.logo_res.try_recv() else { break };
            tv.page.logo_saved(&url);
        }
        let focus = self.focused();
        if matches!(self.mode, Mode::Browse)
            && self.featured(focus).is_some()
            && unsafe { SDL_GetTicks() }.wrapping_sub(self.home_bg_at) >= FEATURED_MS
        {
            self.pick_home_bg(true);
        }
        let focus_key = match self.featured(focus) {
            Some(b) => Some(format!("featured:{}", b.path)),
            None => self.focused_key(),
        };
        if focus_key != self.hero_key {
            self.hero_key = focus_key;
            self.hero_a = 0.0;
        }
        let bg = focus
            .and_then(|(r, c)| self.rows[r].items[c].bg.clone())
            .or_else(|| self.home_bg.as_ref().map(|b| b.path.clone()));
        self.request_bg(bg.as_deref());
        self.take_bg();
        if self
            .ok_down
            .is_some_and(|t| unsafe { SDL_GetTicks() }.wrapping_sub(t) >= HOLD_MS)
        {
            self.ok_down = None;
            self.open_menu();
        }
        let target = self.vpos(self.cur_row) as f32;
        let mut anim = ease(&mut self.row_scroll, target, dt, 12.0);
        anim |= ease(&mut self.hero_a, 1.0, dt, 9.0);
        anim |= ease(&mut self.bg_a[0], 1.0, dt, 6.0);
        anim |= ease(&mut self.bg_a[1], 0.0, dt, 6.0);
        if self.bg_a[1] == 0.0 {
            self.bg[1] = None;
        }
        let fc = focus.map_or(BG, |(r, c)| self.rows[r].items[c].color);
        for (g, v) in self.glow.iter_mut().zip([fc.r, fc.g, fc.b]) {
            anim |= ease(g, f32::from(v), dt, 5.0);
        }
        anim |= ease(&mut self.cog_f, f32::from(self.cog), dt, 14.0);
        let top = self.top();
        if let Some(tv) = &mut self.tv {
            anim |= tv.page.animate(dt);
        }
        for (r, row) in self.rows.iter_mut().enumerate() {
            anim |= ease(&mut row.scroll, row.col as f32 * (card_size(r).0 + GAP), dt, 12.0);
            anim |= ease(&mut row.active, f32::from(r == self.cur_row && !top), dt, 10.0);
            for (i, it) in row.items.iter_mut().enumerate() {
                anim |= ease(
                    &mut it.focus,
                    f32::from(r == self.cur_row && i == row.col && !top),
                    dt,
                    14.0,
                );
            }
        }
        self.animating = anim || self.ok_down.is_some();
        if self.hint_options.is_none() {
            self.hint_options = self.g.text(self.g.f_label, "Hold OK for app options", 0);
            self.hint_move = self
                .g
                .text(self.g.f_label, "Left / Right to move   OK to drop   Back to cancel", 0);
        }
        self.draw(focus);
    }

    fn draw(&mut self, focus: Option<(usize, usize)>) {
        let g = &self.g;
        unsafe {
            SDL_SetRenderDrawColor(g.ren, BG.r, BG.g, BG.b, 255);
            SDL_RenderClear(g.ren);
        }
        let glow = Color {
            r: self.glow[0] as u8,
            g: self.glow[1] as u8,
            b: self.glow[2] as u8,
            a: 255,
        };
        if self.on_tv {
            return self.draw_tv();
        }
        g.tint(&g.soft, glow, 0.45);
        g.copy(
            &g.soft,
            None,
            FRect {
                x: W * 0.25,
                y: -H * 0.9,
                w: W * 1.2,
                h: H * 1.6,
            },
        );
        for l in [1, 0] {
            if let Some(t) = &self.bg[l] {
                g.tint(t, WHITE, self.bg_a[l]);
                g.copy(
                    t,
                    None,
                    FRect {
                        x: 0.0,
                        y: 0.0,
                        w: W,
                        h: H,
                    },
                );
            }
        }
        g.tint(&g.hgrad, BG, 0.82);
        g.copy(
            &g.hgrad,
            None,
            FRect {
                x: 0.0,
                y: 0.0,
                w: W * 0.62,
                h: H,
            },
        );
        g.tint(&g.vgrad, BG, SHELF);
        g.copy(
            &g.vgrad,
            None,
            FRect {
                x: 0.0,
                y: 280.0,
                w: W,
                h: 330.0,
            },
        );
        g.fill(
            BG,
            SHELF,
            FRect {
                x: 0.0,
                y: 610.0,
                w: W,
                h: H,
            },
        );
        g.tint(&g.vgrad, BG, 0.6);
        let top = FRect {
            x: 0.0,
            y: 0.0,
            w: W,
            h: 180.0,
        };
        unsafe {
            SDL_RenderCopyExF(
                g.ren,
                g.vgrad.ptr(),
                std::ptr::null(),
                &top,
                0.0,
                std::ptr::null(),
                FLIP_VERTICAL,
            )
        };

        let featured = self.featured(focus).cloned();
        if let Some(b) = featured {
            if self.home_hero.as_ref().map(|h| &h.0) != Some(&b.path) {
                let sub = g.text(g.f_sub, &b.sub, 900).filter(|_| !b.sub.is_empty());
                self.home_hero = Some((b.path.clone(), g.text(g.f_hero, &b.title, 0), sub));
            }
            if let Some((_, title, sub)) = &self.home_hero {
                let dx = (1.0 - self.hero_a) * 32.0;
                g.put(title.as_ref(), MARGIN + dx, 200.0, TEXT, self.hero_a);
                g.put(sub.as_ref(), MARGIN + dx, 310.0, DIM, self.hero_a);
            }
        } else if let Some((r, c)) = focus {
            let it = &mut self.rows[r].items[c];
            if it.hero.is_none() {
                // A long video title would run across the whole screen.
                let title: String = if it.title.chars().count() > 36 {
                    {
                        let cut: String = it.title.chars().take(35).collect();
                        cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a).to_string() + "…"
                    }
                } else {
                    it.title.clone()
                };
                it.hero = g.text(g.f_hero, &title, 0);
            }
            if it.hero_sub.is_none() {
                it.hero_sub = it.sub.as_deref().and_then(|s| g.text(g.f_sub, s, 900));
            }
            let dx = (1.0 - self.hero_a) * 32.0;
            g.put(it.hero.as_ref(), MARGIN + dx, 200.0, TEXT, self.hero_a);
            g.put(it.hero_sub.as_ref(), MARGIN + dx, 310.0, DIM, self.hero_a);
        }

        self.draw_top();
        let g = &self.g;

        let held = self
            .ok_down
            .map(|t| (unsafe { SDL_GetTicks() }.wrapping_sub(t) as f32 / HOLD_MS as f32).min(1.0));
        let focused_app = focus.is_some_and(|(r, c)| self.rows[r].items[c].kind == Kind::App);
        // Rows differ in height, so their tops add up; the scroll position is a (fractional) row number.
        let mut tops = vec![];
        let mut at = 0.0;
        for r in (0..self.rows.len()).filter(|r| !self.rows[*r].items.is_empty()) {
            tops.push(at);
            at += row_height(r);
        }
        if tops.is_empty() {
            tops.push(0.0);
        }
        let i = (self.row_scroll.floor().max(0.0) as usize).min(tops.len().saturating_sub(1));
        let next = tops.get(i + 1).copied().unwrap_or(tops[i]);
        let scroll_px = tops[i] + (next - tops[i]) * (self.row_scroll - i as f32).clamp(0.0, 1.0);
        let mut pos = 0;
        let top = self.top();
        for (r, row) in self.rows.iter_mut().enumerate() {
            for it in &mut row.items {
                it.rect.w = 0.0;
            }
            if row.items.is_empty() {
                continue;
            }
            let ry = ROW_Y + tops[pos] - scroll_px;
            let ra = clamp(1.0 - (self.row_scroll - pos as f32), 0.0, 1.0);
            pos += 1;
            if ra <= 0.0 || ry > H {
                continue;
            }
            if row.name_tex.is_none() {
                row.name_tex = g.text(g.f_row, row.name, 0);
            }
            let mix = |d: u8, t: u8| (f32::from(d) + (f32::from(t) - f32::from(d)) * row.active) as u8;
            g.put(
                row.name_tex.as_ref(),
                MARGIN,
                ry,
                rgb(mix(DIM.r, TEXT.r), mix(DIM.g, TEXT.g), mix(DIM.b, TEXT.b)),
                ra,
            );
            if r == self.cur_row && focused_app {
                let hint = if matches!(self.mode, Mode::Moving(_)) {
                    self.hint_move.as_ref()
                } else {
                    self.hint_options.as_ref()
                };
                if let Some(h) = hint {
                    let x = W - MARGIN - h.w as f32;
                    g.put(Some(h), x, ry, DIM, ra * row.active);
                }
            }
            let dim = 0.45 * (1.0 - row.active);
            // The focused card is drawn last, and one losing focus before it, so they overlap their neighbours.
            for pass in 0..3 {
                for (i, it) in row.items.iter_mut().enumerate() {
                    let size = card_size(r);
                    let x = MARGIN + i as f32 * (size.0 + GAP) - row.scroll;
                    let own = if r == self.cur_row && i == row.col && !top {
                        2
                    } else {
                        i32::from(it.focus > 0.01)
                    };
                    if pass == own && x <= W && x + size.0 * 1.2 >= 0.0 {
                        g.card(it, size, x, ry + LABEL_H, ra, dim);
                    }
                }
            }
            if let (Some(frac), true) = (held, r == self.cur_row) {
                if let Some(q) = row.items.get(row.col).map(|i| i.rect).filter(|q| q.w > 0.0) {
                    g.rounded(q.x + 20.0, q.y + q.h - 22.0, (q.w - 40.0) * frac, 8.0, 4.0, WHITE, 0.95);
                }
            }
        }
        match &self.mode {
            Mode::Menu(m) => draw_menu(g, m),
            Mode::Login(l) => l.draw(g),
            Mode::Settings(p) => p.draw(g),
            Mode::Prompt(p, _) => p.draw(g),
            _ => {}
        }
    }

    /// The clock, the cog and, once live TV is set up, the Home and TV tabs.
    fn draw_top(&mut self) {
        if self.tv.is_some() && self.tab_tex[0].is_none() {
            self.tab_tex = [self.g.text(self.g.f_row, "Home", 0), self.g.text(self.g.f_row, "TV", 0)];
        }
        let g = &self.g;
        let (hour, min) = local_time();
        if min != self.clock_min {
            self.clock = g.text(g.f_clock, &format!("{hour:02}:{min:02}"), 0);
            self.clock_min = min;
        }
        let cw = self.clock.as_ref().map_or(0.0, |t| t.w as f32);
        g.put(self.clock.as_ref(), W - MARGIN - cw, 56.0, TEXT, 1.0);
        let ch = self.clock.as_ref().map_or(40.0, |t| t.h as f32);
        let (cx, cy) = (W - MARGIN - cw - 76.0, 56.0 + ch / 2.0);
        g.rounded(cx - 36.0, cy - 36.0, 72.0, 72.0, 36.0, WHITE, self.cog_f);
        let gc = rgb(
            (f32::from(DIM.r) * (1.0 - self.cog_f)) as u8,
            (f32::from(DIM.g) * (1.0 - self.cog_f)) as u8,
            (f32::from(DIM.b) * (1.0 - self.cog_f)) as u8,
        );
        g.tint(&g.gear, gc, 1.0);
        g.copy(
            &g.gear,
            None,
            FRect {
                x: cx - 22.0,
                y: cy - 22.0,
                w: 44.0,
                h: 44.0,
            },
        );

        if self.tv.is_none() {
            return;
        }
        let widths: Vec<f32> = self
            .tab_tex
            .iter()
            .map(|t| t.as_ref().map_or(0.0, |t| t.w as f32) + 64.0)
            .collect();
        let mut x = (W - widths.iter().sum::<f32>() - 16.0) / 2.0;
        for (i, t) in self.tab_tex.iter().enumerate() {
            let current = usize::from(self.on_tv) == i;
            let th = t.as_ref().map_or(0.0, |t| t.h as f32);
            let colour = if current && self.tabs_f {
                g.rounded(x, cy - 32.0, widths[i], 64.0, 32.0, WHITE, 1.0);
                BLACK
            } else if current {
                g.rounded(x + 32.0, cy + th / 2.0 + 4.0, widths[i] - 64.0, 4.0, 2.0, TEXT, 1.0);
                TEXT
            } else {
                DIM
            };
            g.put(t.as_ref(), x + 32.0, cy - th / 2.0, colour, 1.0);
            x += widths[i] + 16.0;
        }
    }

    fn draw_tv(&mut self) {
        self.draw_top();
        let active = !self.top() && matches!(self.mode, Mode::Browse);
        if let Some(tv) = &mut self.tv {
            tv.page.draw(&self.g, &tv.list, &tv.guide, active);
        }
        let g = &self.g;
        g.put(self.tv_note.as_ref(), MARGIN, H - 80.0, YELLOW, 1.0);
        match &self.mode {
            Mode::Menu(m) => draw_menu(g, m),
            Mode::Settings(p) => p.draw(g),
            Mode::Prompt(p, _) => p.draw(g),
            Mode::Login(l) => l.draw(g),
            _ => {}
        }
    }

    fn nav(&mut self, dx: i32, dy: i32) {
        if self.cog {
            self.cog = dy <= 0;
            return;
        }
        let row = &mut self.rows[self.cur_row];
        let col = row.col as i32 + dx;
        if dx != 0 && col >= 0 && (col as usize) < row.items.len() {
            row.col = col as usize;
        }
        let mut n = self.cur_row as i32 + dy;
        while dy != 0 && n >= 0 && (n as usize) < self.rows.len() {
            if !self.rows[n as usize].items.is_empty() {
                self.cur_row = n as usize;
                break;
            }
            n += dy;
        }
        // Up from the top row reaches the tabs, or the cog when there are none.
        if dy < 0 && self.cur_row == self.first_row() {
            self.focus_top();
        }
    }

    /// The focus is on the bar at the top: the tabs or the cog.
    fn top(&self) -> bool {
        self.cog || self.tabs_f
    }

    fn focus_top(&mut self) {
        if self.tv.is_some() {
            self.tabs_f = true;
        } else {
            self.cog = true;
        }
    }

    /// The keys while the focus is on the top bar: Left and Right go between Home, TV and the cog.
    fn top_key(&mut self, dx: i32, dy: i32, ok: bool) {
        if self.cog {
            if ok {
                self.open_settings();
            } else if dy > 0 {
                self.cog = false;
            } else if dx < 0 && self.tv.is_some() {
                self.cog = false;
                self.tabs_f = true;
            }
            return;
        }
        if dy > 0 || ok {
            self.tabs_f = false;
            return;
        }
        let to = i32::from(self.on_tv) + dx;
        if to > 1 {
            self.tabs_f = false;
            self.cog = true;
        } else if to >= 0 {
            self.on_tv = to == 1;
        }
    }

    fn activate(&mut self, r: usize, c: usize) {
        match self.rows[r].items[c].kind {
            Kind::Toggle => {
                self.show_hidden = !self.show_hidden;
                let (key, col) = (self.focused_key(), self.rows[APPS].col);
                self.rebuild_apps();
                self.refocus(key, col);
            }
            _ => self.luna.launch(&self.rows[r].items[c]),
        }
    }

    fn show_menu(&mut self, title: &str, list: Vec<(Opt, String)>, sel: usize) {
        let opts = list
            .into_iter()
            .map(|(o, s)| (o, self.g.text(self.g.f_menu, &s, 0)))
            .collect();
        self.mode = Mode::Menu(Menu {
            title: self.g.text(self.g.f_label, title, 0),
            opts,
            sel,
        });
    }

    fn open_menu(&mut self) {
        if self.on_tv {
            let Some(tv) = &self.tv else { return };
            let Some(i) = tv.page.channel() else { return };
            let (name, key) = (tv.list[i].name.clone(), tv.list[i].key());
            let mut list = vec![(Opt::TvDetails, "Programme details".to_string())];
            list.push(if self.prefs.tv_favs.contains(&key) {
                (Opt::Unfav(key), "Remove from Favourites".to_string())
            } else {
                (Opt::Fav(key), "Add to Favourites".to_string())
            });
            return self.show_menu(&name, list, 0);
        }
        let Some((r, c)) = self.focused() else { return };
        let it = &self.rows[r].items[c];
        if it.kind != Kind::App {
            return;
        }
        let list = if it.hidden {
            vec![(Opt::Show, "Show on Home".to_string())]
        } else {
            let pin = if self.prefs.pinned.contains(&it.key) {
                (Opt::Unpin, "Unpin".to_string())
            } else {
                (Opt::Pin, "Pin to Home".into())
            };
            let mut list = vec![(Opt::Move, "Move".into()), pin, (Opt::Hide, "Hide from Home".into())];
            if it.removable {
                list.push((Opt::AskUninstall, "Uninstall".into()));
            }
            list
        };
        let title = it.title.clone();
        self.show_menu(&title, list, 0);
    }

    fn do_setting(&mut self, act: Act) {
        self.last_settings = match &self.mode {
            Mode::Settings(p) => (p.cat, p.row, p.pane),
            _ => self.last_settings,
        };
        if act == Act::Boot {
            set_boot(&self.luna.root, !boot_on(&self.luna.root));
            return self.show_settings();
        }
        if let Act::Debloat(group) = act {
            let on = &mut self.prefs.debloat;
            match on.iter().position(|g| g == group) {
                Some(i) => drop(on.remove(i)),
                None => on.push(group.into()),
            }
            self.prefs.save();
            apply_debloat(&self.luna.root);
            return self.show_settings();
        }
        if act == Act::TvSource {
            let mut list = vec![
                (Opt::AskM3u, "M3U playlist address".to_string()),
                (Opt::AskXtream, "Xtream Codes login".into()),
            ];
            if self.tv.is_some() {
                list.push((Opt::RefreshTv, "Refresh now".into()));
                list.push((Opt::RemoveTv, "Remove".into()));
            }
            return self.show_menu("Live TV channels", list, 0);
        }
        if act == Act::TvGuide {
            if self.tv.is_none() {
                return self.show_menu("Set up the channels first", vec![(Opt::Close, "OK".into())], 0);
            }
            let now = self.tv.as_ref().and_then(|t| t.cfg.guide.clone()).unwrap_or_default();
            return self.ask(
                Ask::Guide,
                "Guide",
                "The address of an XMLTV guide (.xml or .xml.gz). Leave it empty to use the one the channels bring.",
                "http://example.com/guide.xml",
                &now,
            );
        }
        if act == Act::Plex {
            if let Some(px) = &self.px {
                let title = format!("Plex: {}", px.cfg.user);
                let list = vec![
                    (Opt::RefreshPlex, "Refresh now".to_string()),
                    (Opt::PlexSignOut, "Sign out".into()),
                ];
                self.show_menu(&title, list, 0);
            } else {
                let l = Login::plex(&self.g, &self.luna.root, self.wake);
                self.mode = Mode::Login(Box::new(l));
            }
        } else if act == Act::PlexApp {
            let current = self.plex_app();
            let mut list: Vec<(Opt, String)> = self
                .all_apps()
                .filter(|i| {
                    let name = format!("{} {}", i.id, i.title).to_lowercase();
                    i.id == current || ["plex", "plx"].iter().any(|k| name.contains(k))
                })
                .map(|i| {
                    let mark = if i.id == current { "   (current)" } else { "" };
                    (Opt::UsePlexApp(i.id.clone()), format!("{}{mark}", i.title))
                })
                .collect();
            let sel = list
                .iter()
                .position(|(o, _)| matches!(o, Opt::UsePlexApp(id) if *id == current))
                .unwrap_or(0);
            if list.is_empty() {
                list.push((Opt::Close, "No Plex app is installed".into()));
            }
            self.show_menu("Open Plex items in", list, sel);
        } else if act == Act::Youtube {
            if self.yt.is_some() {
                let list = vec![
                    (Opt::RefreshYoutube, "Refresh now".to_string()),
                    (Opt::YoutubeSignOut, "Sign out".into()),
                ];
                self.show_menu("YouTube", list, 0);
            } else {
                let l = Login::youtube(&self.g, &self.luna.root, self.wake);
                self.mode = Mode::Login(Box::new(l));
            }
        } else if act == Act::Jellyfin {
            if let Some(jf) = &self.jf {
                let title = format!("Jellyfin: {}", jf.cfg.user);
                let list = vec![
                    (Opt::RefreshJellyfin, "Refresh now".to_string()),
                    (Opt::SignOut, "Sign out".into()),
                ];
                self.show_menu(&title, list, 0);
            } else {
                let l = Login::new(&self.g, &self.luna.root, self.wake, self.prefs.jf_server.clone());
                self.mode = Mode::Login(Box::new(l));
            }
        } else {
            // Jellyfin clients: Native Jelly (any build), Moonfin, the official app.
            let current = self.jf_app();
            let mut list: Vec<(Opt, String)> = self
                .all_apps()
                .filter(|i| {
                    let name = format!("{} {}", i.id, i.title).to_lowercase();
                    i.id == current || ["jelly", "moonfin", "moonfish"].iter().any(|k| name.contains(k))
                })
                .map(|i| {
                    let mark = if i.id == current { "   (current)" } else { "" };
                    (Opt::UseApp(i.id.clone()), format!("{}{mark}", i.title))
                })
                .collect();
            let sel = list
                .iter()
                .position(|(o, _)| matches!(o, Opt::UseApp(id) if *id == current))
                .unwrap_or(0);
            if list.is_empty() {
                list.push((Opt::Close, "No Jellyfin app is installed".into()));
            }
            self.show_menu("Open Jellyfin items in", list, sel);
        }
    }

    fn run_option(&mut self, opt: &Opt) {
        let (key, col) = (self.focused_key().unwrap_or_default(), self.rows[self.cur_row].col);
        match opt {
            Opt::Move => {
                let row = &mut self.rows[self.cur_row];
                let orig = row.items.iter().map(|i| i.key.clone()).collect();
                row.items[col].moving = true;
                self.mode = Mode::Moving(orig);
                return;
            }
            Opt::Pin => {
                self.prefs.pinned.push(key.clone());
                self.prefs.save();
                self.rebuild_apps();
                return self.refocus(Some(key), col);
            }
            Opt::Unpin => {
                self.prefs.pinned.retain(|k| *k != key);
                self.prefs.save();
                self.rebuild_apps();
                return self.refocus(Some(key), col);
            }
            Opt::Hide => {
                self.prefs.pinned.retain(|k| *k != key);
                self.prefs.hidden.push(key);
            }
            Opt::Show => self.prefs.hidden.retain(|k| *k != key),
            Opt::RefreshJellyfin => return self.refresh_jellyfin(),
            Opt::SignOut => return self.sign_out(),
            Opt::RefreshYoutube => return self.refresh_youtube(true),
            Opt::YoutubeSignOut => return self.sign_out_youtube(),
            Opt::RefreshPlex => return self.refresh_plex(),
            Opt::PlexSignOut => return self.sign_out_plex(),
            Opt::UsePlexApp(id) => {
                self.prefs.plex_app = Some(id.clone());
                self.prefs.save();
                for it in &mut self.rows[PLEX].items {
                    it.id = id.clone();
                }
                return;
            }
            Opt::UseApp(id) => {
                self.prefs.jf_app = Some(id.clone());
                self.prefs.save();
                for it in &mut self.rows[WATCH].items {
                    it.id = id.clone();
                }
                return;
            }
            Opt::AskM3u => {
                let now = match self.tv.as_ref().map(|t| &t.cfg.source) {
                    Some(iptv::Source::M3u(url)) => url.clone(),
                    _ => String::new(),
                };
                return self.ask(
                    Ask::M3u,
                    "M3U playlist",
                    "The address of your playlist. It is kept on this TV only.",
                    "http://example.com/playlist.m3u",
                    &now,
                );
            }
            Opt::AskXtream => {
                let now = match self.tv.as_ref().map(|t| &t.cfg.source) {
                    Some(iptv::Source::Xtream { server, .. }) => server.clone(),
                    _ => String::new(),
                };
                return self.ask(
                    Ask::XtServer,
                    "Xtream Codes login (1 of 3)",
                    "The server's address, with its port.",
                    "http://example.com:8080",
                    &now,
                );
            }
            Opt::RefreshTv => return self.refresh_tv(true),
            Opt::RemoveTv => return self.remove_tv(),
            Opt::Fav(k) => {
                self.prefs.tv_favs.push(k.clone());
                self.prefs.save();
                return self.rebuild_tv();
            }
            Opt::TvDetails => {
                if let Some(tv) = &mut self.tv {
                    tv.page.open_details(&tv.list, &tv.guide);
                }
            }
            Opt::Unfav(k) => {
                self.prefs.tv_favs.retain(|f| f != k);
                self.prefs.save();
                return self.rebuild_tv();
            }
            Opt::AskUninstall => {
                let title = format!("Uninstall {}?", self.rows[self.cur_row].items[col].title);
                let list = vec![(Opt::Uninstall, "Uninstall".to_string()), (Opt::Close, "Cancel".into())];
                return self.show_menu(&title, list, 1);
            }
            Opt::Uninstall => {
                self.prefs.hidden.retain(|k| *k != key);
                self.prefs.pinned.retain(|k| *k != key);
                self.prefs.order.retain(|k| *k != key);
                self.rm_rx = self.luna.uninstall(&key, self.wake);
                for r in [PINNED, APPS] {
                    self.rows[r].items.retain(|i| i.key != key);
                }
                self.pool.retain(|i| i.key != key);
            }
            Opt::Close => return,
        }
        self.prefs.save();
        self.rebuild_apps();
        self.refocus(None, col);
    }

    fn ask(&mut self, ask: Ask, title: &str, body: &str, hint: &str, text: &str) {
        let secret = matches!(ask, Ask::XtPass(..));
        let p = Prompt::new(&self.g, title, body, hint, text, secret);
        self.mode = Mode::Prompt(Box::new(p), ask);
    }

    /// A prompt was answered: the next one of the Xtream login, or the change saved.
    fn answered(&mut self, ask: Ask, text: String) {
        let url = |t: &str| {
            if t.contains("://") {
                t.to_string()
            } else {
                format!("http://{t}")
            }
        };
        match ask {
            Ask::M3u if !text.is_empty() => self.set_tv(Some(iptv::Source::M3u(url(&text))), None),
            Ask::XtServer if !text.is_empty() => {
                let user = match self.tv.as_ref().map(|t| &t.cfg.source) {
                    Some(iptv::Source::Xtream { user, .. }) => user.clone(),
                    _ => String::new(),
                };
                let server = url(&text).trim_end_matches('/').to_string();
                return self.ask(
                    Ask::XtUser(server),
                    "Xtream Codes login (2 of 3)",
                    "Your user name.",
                    "user name",
                    &user,
                );
            }
            Ask::XtUser(server) if !text.is_empty() => {
                return self.ask(
                    Ask::XtPass(server, text),
                    "Xtream Codes login (3 of 3)",
                    "Your password. It is kept on this TV only.",
                    "password",
                    "",
                );
            }
            Ask::XtPass(server, user) if !text.is_empty() => self.set_tv(
                Some(iptv::Source::Xtream {
                    server,
                    user,
                    pass: text,
                }),
                None,
            ),
            Ask::Guide => self.set_tv(None, Some((!text.is_empty()).then(|| url(&text)))),
            _ => {}
        }
        self.close_dialog();
    }

    fn move_key(&mut self, dx: i32, ok: bool, back: bool) {
        let Mode::Moving(orig) = &self.mode else { return };
        let row = &mut self.rows[self.cur_row];
        let Some(from) = row.items.iter().position(|i| i.moving) else {
            return self.mode = Mode::Browse;
        };
        if !ok && !back {
            let to = from as i32 + dx;
            if to >= 0 && (to as usize) < row.items.len() && row.items[to as usize].kind != Kind::Toggle {
                row.items.swap(from, to as usize);
                row.col = to as usize;
            }
            return;
        }
        let key = row.items[from].key.clone();
        if back {
            let pos = |k: &str| orig.iter().position(|o| o == k).unwrap_or(usize::MAX);
            row.items.sort_by_key(|i| pos(&i.key));
        }
        row.items.iter_mut().for_each(|i| i.moving = false);
        row.col = row.items.iter().position(|i| i.key == key).unwrap_or(0);
        if ok {
            let mut order: Vec<String> = row
                .items
                .iter()
                .filter(|i| i.kind != Kind::Toggle)
                .map(|i| i.key.clone())
                .collect();
            if self.cur_row == PINNED {
                self.prefs.pinned = order;
            } else {
                order.extend(self.pool.iter().map(|i| i.key.clone()));
                self.prefs.order = order;
            }
            self.prefs.save();
        }
        self.mode = Mode::Browse;
    }

    /// False when the launcher should exit.
    pub fn handle(&mut self, e: &Event) -> bool {
        self.touched |= matches!(e.kind(), KEYDOWN | MOUSEBUTTONUP);
        match e.kind() {
            QUIT => return false,
            KEYUP if matches!(e.key_sym(), K_RETURN | K_KP_ENTER) => {
                log("key up: OK");
                self.ok_up();
            }
            KEYDOWN => {
                let (sym, wcode) = (e.key_sym(), e.key_wcode());
                log(&format!("key sym={sym} wcode={wcode}"));
                return self.key(sym, wcode, e.key_repeat());
            }
            // The Magic Remote's pointer is not used: moving it does nothing, and its click is OK.
            MOUSEBUTTONDOWN if e.0[16] == BUTTON_LEFT => return self.key(K_RETURN, 0, false),
            MOUSEBUTTONUP if e.0[16] == BUTTON_LEFT => self.ok_up(),
            WINDOWEVENT => {
                // Coming back to the front: pick up installed or removed apps, input changes and new progress.
                let now = unsafe { SDL_GetTicks() };
                if e.0[12] == WINDOWEVENT_FOCUS_GAINED && now.wrapping_sub(self.loaded_at) > 3000 {
                    self.load();
                    self.pick_home_bg(true);
                    self.refresh_jellyfin();
                    self.refresh_youtube(false);
                    self.refresh_plex();
                    self.refresh_tv(false);
                    self.loaded_at = now;
                }
            }
            _ => {}
        }
        true
    }

    fn key(&mut self, sym: i32, wcode: u32, repeat: bool) -> bool {
        let back = sym == K_ESCAPE || wcode == WCODE_BACK;
        let options = sym == K_M || wcode == WCODE_YELLOW;
        let enter = matches!(sym, K_RETURN | K_KP_ENTER);
        let ok = !repeat && enter;
        let dir = |l: i32, r: i32| i32::from(sym == r) - i32::from(sym == l);
        let (dx, dy) = (dir(K_LEFT, K_RIGHT), dir(K_UP, K_DOWN));
        if !enter {
            self.ok_down = None;
        }
        match &mut self.mode {
            Mode::Browse => {
                let details = self.tv.as_ref().is_some_and(|t| self.on_tv && t.page.details);
                // The programme details: Back closes them, OK (below) watches the channel.
                if details && (back || options || dx != 0 || dy != 0) {
                    if let (true, Some(tv)) = (back, &mut self.tv) {
                        tv.page.details = false;
                    }
                    return true;
                }
                if back && self.on_tv && !self.top() {
                    if let Some(tv) = &mut self.tv {
                        if tv.page.rewind(&tv.list, &tv.guide) {
                            return true;
                        }
                    }
                }
                if back && self.on_tv {
                    self.on_tv = false;
                    self.tabs_f = false;
                    return true;
                }
                if back {
                    // BACK on the TV: Home has nowhere to go back to.
                    return self.luna.root.is_empty();
                }
                if self.top() {
                    self.top_key(dx, dy, ok);
                } else if self.on_tv {
                    self.tv_key(dx, dy, ok, options);
                } else if options {
                    self.open_menu();
                } else if ok {
                    match self.focused() {
                        // An app waits for OK to be released: a tap launches it, a hold opens its options.
                        Some((r, c)) if self.rows[r].items[c].kind == Kind::App => {
                            self.ok_down = self.ok_down.or(Some(unsafe { SDL_GetTicks() }));
                        }
                        Some((r, c)) => self.activate(r, c),
                        None => {}
                    }
                } else {
                    self.nav(dx, dy);
                }
            }
            Mode::Menu(m) => {
                if back || options {
                    self.close_dialog();
                } else if ok {
                    self.menu_choose();
                } else {
                    m.sel = (m.sel as i32 + dy).clamp(0, m.opts.len() as i32 - 1) as usize;
                }
            }
            Mode::Moving(_) => self.move_key(dx, ok, back),
            Mode::Login(l) => {
                if !l.key(&self.g, sym, ok, back, dx, dy) {
                    self.close_dialog();
                }
            }
            Mode::Settings(p) => {
                let out = p.key(dx, dy, ok, back);
                self.settings_out(out);
            }
            Mode::Prompt(p, _) => match p.key(&self.g, sym, ok, back, dx, dy) {
                Typed::Stay => {}
                Typed::Cancel => self.close_dialog(),
                Typed::Done(text) => {
                    if let Mode::Prompt(_, ask) = std::mem::replace(&mut self.mode, Mode::Browse) {
                        self.answered(ask, text);
                    }
                }
            },
        }
        true
    }

    /// The keys on the TV tab. OK waits for its release: a tap plays the channel (or shows a later programme's
    /// details), a hold opens the options.
    fn tv_key(&mut self, dx: i32, dy: i32, ok: bool, options: bool) {
        let Some(tv) = &mut self.tv else { return };
        if options {
            return self.open_menu();
        }
        if ok && tv.page.channel().is_some() {
            self.ok_down = self.ok_down.or(Some(unsafe { SDL_GetTicks() }));
            return;
        }
        let row = tv.page.row();
        match tv.page.key(&tv.list, &tv.guide, dx, dy) {
            tv::Out::Top => self.tabs_f = true,
            tv::Out::Ok(i) => self.watch(i),
            tv::Out::Stay => {}
        }
        if let Some(tv) = &self.tv {
            if tv.page.row() != row {
                let _ = tv.logo_req.send(tv.page.logos(&tv.list));
            }
        }
    }

    /// For testing without the remote: plays what `player::TRIGGER` names, a channel number or a stream address.
    pub fn play_file(&mut self) {
        let Ok(what) = std::fs::read_to_string(crate::player::TRIGGER) else {
            return;
        };
        let _ = std::fs::remove_file(crate::player::TRIGGER);
        let what = what.trim();
        if let Ok(n) = what.parse::<usize>() {
            self.on_tv = true;
            if let Some(tv) = &mut self.tv {
                while tv.list.is_empty() {
                    match tv.res.recv_timeout(std::time::Duration::from_secs(30)) {
                        Ok(Got::Channels(list)) => tv.list = list,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }
            self.rebuild_tv();
            return self.watch(n.saturating_sub(1));
        }
        let ch = Channel {
            name: "Test".into(),
            group: String::new(),
            url: what.into(),
            logo: None,
            epg: String::new(),
        };
        if self.player.is_none() {
            self.player = crate::player::Player::new()
                .map_err(|e| log(&format!("play: {e}")))
                .ok();
        }
        if let Some(p) = &mut self.player {
            p.watch(&self.g, &[ch], 0, &Guide::new());
        }
    }

    fn menu_choose(&mut self) {
        let Mode::Menu(m) = std::mem::replace(&mut self.mode, Mode::Browse) else {
            return;
        };
        self.run_option(&m.opts[m.sel].0);
        if matches!(self.mode, Mode::Browse) && self.in_settings {
            self.show_settings();
        }
    }

    fn settings_out(&mut self, out: Out) {
        match out {
            Out::Stay => {}
            Out::Back => {
                self.in_settings = false;
                self.mode = Mode::Browse;
            }
            Out::Do(act) => self.do_setting(act),
        }
    }

    /// OK was released: a tap on an app launches it (a hold already opened its options).
    fn ok_up(&mut self) {
        if self.on_tv {
            if let (Some(_), Mode::Browse) = (self.ok_down.take(), &self.mode) {
                let out = self.tv.as_mut().map(|t| t.page.tap(&t.list, &t.guide));
                if let Some(tv::Out::Ok(i)) = out {
                    self.watch(i);
                }
            }
            return;
        }
        if self.ok_down.take().is_some() && matches!(self.mode, Mode::Browse) {
            if let Some((r, c)) = self.focused() {
                self.activate(r, c);
            }
        }
    }

    /// The Home key while the launcher is already running (see `lifecycle::register`): bring the window back and start over.
    pub fn relaunch(&mut self) {
        unsafe {
            SDL_ShowWindow(self.win);
            SDL_RaiseWindow(self.win);
        }
        self.mode = Mode::Browse;
        self.in_settings = false;
        self.ok_down = None;
        self.cog = false;
        self.tabs_f = false;
        self.on_tv = false;
        self.refresh_tv(false);
        self.load();
        for r in &mut self.rows {
            r.col = 0;
        }
        self.cur_row = self.first_row();
        self.settle();
        self.pick_home_bg(true);
        self.refresh_jellyfin();
        self.refresh_youtube(false);
        self.refresh_plex();
    }
}

/// The panel of a menu and where its options are.
fn menu_panel(m: &Menu) -> (f32, f32, f32, f32) {
    let wide = m
        .opts
        .iter()
        .filter_map(|(_, t)| t.as_ref())
        .map(|t| t.w as f32)
        .fold(0.0, f32::max);
    let (pw, ph) = ((wide + 112.0).max(640.0), 124.0 + m.opts.len() as f32 * 84.0);
    ((W - pw) / 2.0, (H - ph) / 2.0, pw, ph)
}

fn draw_menu(g: &Gfx, m: &Menu) {
    g.fill(
        BLACK,
        0.6,
        FRect {
            x: 0.0,
            y: 0.0,
            w: W,
            h: H,
        },
    );
    let (px, py, pw, ph) = menu_panel(m);
    g.rounded(px, py, pw, ph, 28.0, PANEL, 1.0);
    g.put(m.title.as_ref(), px + 48.0, py + 36.0, DIM, 1.0);
    for (i, (_, tex)) in m.opts.iter().enumerate() {
        let oy = py + 100.0 + i as f32 * 84.0;
        let th = tex.as_ref().map_or(0.0, |t| t.h as f32);
        if i == m.sel {
            g.rounded(px + 24.0, oy, pw - 48.0, 72.0, 18.0, WHITE, 1.0);
        }
        g.put(
            tex.as_ref(),
            px + 56.0,
            oy + (72.0 - th) / 2.0,
            if i == m.sel { BLACK } else { TEXT },
            1.0,
        );
    }
}

fn local_time() -> (i32, i32) {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        (tm.tm_hour, tm.tm_min)
    }
}
