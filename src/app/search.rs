use super::*;
use crate::icons::Icon;

const COLS: usize = 4;
const VISIBLE: usize = 2 * COLS;
const POSTER: (f32, f32) = (196.0, 294.0);

pub(super) struct Search {
    from_tab: String,
    keyboard: Prompt,
    heading: Option<Tex>,
    query: String,
    dirty: bool,
    hits: Vec<Hit>,
    cards: HashMap<usize, Item>,
    sel: Option<usize>,
    first: usize,
    origin: (f32, f32),
    pub(super) expand: f32,
}

struct Hit {
    tab: String,
    id: String,
    name: String,
    plex: bool,
    poster: Option<(Option<String>, String)>,
}

fn matches<'a, T>(
    query: &str,
    titles: impl Iterator<Item = (&'a str, &'a str, &'a str, Option<&'a str>, T)>,
) -> Vec<T> {
    let query = query.to_lowercase();
    if query.is_empty() {
        return vec![];
    }
    let mut hits: Vec<_> = titles
        .filter_map(|(tab, id, name, key, hit)| {
            let name = name.to_lowercase();
            let rank = if name == query {
                0
            } else if name.starts_with(&query) {
                1
            } else if name.contains(&query) {
                2
            } else {
                return None;
            };
            Some(((rank, name, tab, id), key, hit))
        })
        .collect();
    hits.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut seen = std::collections::HashSet::new();
    hits.into_iter()
        .filter_map(|(_, key, hit)| key.is_none_or(|key| seen.insert(key)).then_some(hit))
        .collect()
}

fn grid_step(i: usize, count: usize, dx: i32, dy: i32) -> Option<usize> {
    if dx < 0 && i.is_multiple_of(COLS) {
        return None;
    }
    if (dx > 0 && i % COLS == COLS - 1) || (dy > 0 && i / COLS == (count - 1) / COLS) {
        return Some(i);
    }
    Some(i.saturating_add_signed((dx + dy * COLS as i32) as isize).min(count - 1))
}

impl Search {
    pub(super) fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub(super) fn saved(&mut self, path: &str) {
        for card in self.cards.values_mut() {
            if card.icons.first().is_some_and(|p| p == path) {
                card.icon_tried = false;
            }
        }
    }

    pub(super) fn pictures(&self, lib: &mut Lib) {
        let key = format!("search:{}/{}", self.first, self.query);
        if lib.asked != key {
            let _ = lib.pics.send(
                self.hits
                    .iter()
                    .skip(self.first)
                    .take(VISIBLE)
                    .filter(|h| h.plex == lib.plex)
                    .filter_map(|h| {
                        let (url, path) = h.poster.as_ref()?;
                        Some((url.clone()?, path.clone()))
                    })
                    .collect(),
            );
            lib.asked = key;
        }
    }

    pub(super) fn draw(&mut self, g: &Gfx) {
        let p = self.expand;
        let t = 1.0 - (1.0 - p).powi(3);
        g.fill(
            BG,
            t,
            FRect {
                x: 0.0,
                y: 0.0,
                w: W,
                h: H,
            },
        );
        let mix = |a, b| a + (b - a) * t;
        let x = mix(self.origin.0, MARGIN);
        let y = mix(self.origin.1 - 32.0, 68.0);
        let w = mix(64.0, W - 2.0 * MARGIN);
        let h = mix(64.0, 102.0);
        g.rounded(x, y, w, h, mix(32.0, 22.0), WHITE, 1.0 - 0.78 * t);
        g.rounded(x + 2.0, y + 2.0, w - 4.0, h - 4.0, mix(30.0, 20.0), PANEL, t);
        let cy = y + h / 2.0;
        let ix = x + mix(13.0, 28.0);
        let ink = if p < 0.15 { BLACK } else { TEXT };
        g.icon(Icon::Search, ix, cy - 19.0, 38.0, ink, 1.0);
        let label_alpha = ((t - 0.6) / 0.4).max(0.0);
        if label_alpha > 0.0 {
            self.keyboard.draw_search_text(g, x + 92.0, cy, label_alpha);
        }
        if p < 1.0 {
            return;
        }
        let rx = MARGIN + 816.0;
        g.put(self.heading.as_ref(), rx, 220.0, TEXT, 1.0);
        self.keyboard
            .draw_keys(g, (MARGIN, 288.0), (68.0, 76.0), self.sel.is_none());
        self.cards
            .retain(|&i, _| (self.first..self.first + VISIBLE).contains(&i));
        for (n, hit) in self.hits.iter().skip(self.first).take(VISIBLE).enumerate() {
            let i = self.first + n;
            let x = rx + (n % COLS) as f32 * 222.0;
            let y = 288.0 + (n / COLS) as f32 * 378.0;
            let card = self.cards.entry(i).or_insert_with(|| {
                let mut card = Item::new(hit.id.clone(), String::new(), Kind::Watch, Some(hit.name.clone()));
                card.cover = true;
                card.color = PANEL;
                card.icons.extend(hit.poster.iter().map(|(_, path)| path.clone()));
                card.label = g.text(g.f_small, &jellyfin::clip(&hit.name, 24), POSTER.0 as u32 - 8);
                card.hero = g.text(g.f_small, if hit.plex { "Plex" } else { "Jellyfin" }, 0);
                card
            });
            let on = self.sel == Some(i);
            if on {
                g.rounded(x - 5.0, y - 5.0, POSTER.0 + 10.0, POSTER.1 + 10.0, 14.0, WHITE, 1.0);
            }
            g.card(card, POSTER, x, y, 1.0, 0.0);
            let label_y = y + POSTER.1 + 9.0;
            let label = card.label.as_ref().filter(|t| card.icon.is_some() && t.h <= 64);
            g.put(label, x, label_y, TEXT, 1.0);
            g.put(
                card.hero.as_ref(),
                x,
                label_y + label.map_or(0.0, |t| t.h as f32),
                DIM,
                1.0,
            );
        }
    }
}

impl App {
    pub(super) fn open_search(&mut self) {
        self.search = Some(Search {
            from_tab: self.tab.clone(),
            keyboard: Prompt::search(&self.g),
            heading: None,
            query: String::new(),
            dirty: true,
            hits: vec![],
            cards: HashMap::new(),
            sel: None,
            first: 0,
            origin: self.search_origin,
            expand: 0.0,
        });
        self.detail = None;
        self.detail_back.clear();
        self.mode = Mode::Search;
    }

    pub(super) fn update_search(&mut self) {
        if !matches!(self.mode, Mode::Search) || !self.search.as_ref().is_some_and(|s| s.dirty) {
            return;
        }
        let tabs = self.tabs();
        let s = self.search.as_mut().unwrap();
        s.dirty = false;
        let query = s.keyboard.text().trim();
        let changed = query != s.query;
        if changed {
            s.query = query.to_string();
            s.sel = None;
            s.first = 0;
        }
        let titles = tabs.iter().flat_map(|(tab, _)| {
            self.libs
                .iter()
                .filter_map(move |lib| Some((lib.plex, lib.pages.get(tab)?)))
                .flat_map(move |(plex, page)| {
                    page.titles().iter().map(move |t| {
                        (
                            tab.as_str(),
                            t.id.as_str(),
                            t.name.as_str(),
                            t.key.as_deref(),
                            (tab, plex, page, t),
                        )
                    })
                })
        });
        let hits = matches(query, titles);
        let count = hits.len();
        // ponytail: keep the best 64 results; add paging beyond that if narrowing the query is not enough.
        s.hits = hits
            .into_iter()
            .take(64)
            .map(|(tab, plex, page, title)| Hit {
                tab: tab.clone(),
                id: title.id.clone(),
                name: title.name.clone(),
                plex,
                poster: page.poster(title),
            })
            .collect();
        s.cards.clear();
        if let Some(i) = &mut s.sel {
            if s.hits.is_empty() {
                s.sel = None;
                s.first = 0;
            } else {
                *i = (*i).min(s.hits.len() - 1);
                s.first = s.first.min(*i / COLS * COLS);
            }
        }
        let label = if query.is_empty() {
            if self.libs.iter().any(|lib| {
                tabs.iter()
                    .any(|(k, _)| lib.pages.get(k).is_some_and(|p| !p.titles().is_empty()))
            }) {
                "Type a title to see results".into()
            } else {
                "No saved library titles yet".into()
            }
        } else {
            match count {
                0 => "No matching titles".into(),
                1 => "1 result".into(),
                2..=64 => format!("{count} results"),
                _ => format!("64 of {count} results · Refine your search"),
            }
        };
        s.heading = self.g.text(self.g.f_row, &label, 0);
        for lib in &mut self.libs {
            lib.asked.clear();
        }
    }

    pub(super) fn search_key(&mut self, sym: i32, dx: i32, dy: i32, ok: bool, back: bool) {
        if ok || dx != 0 || dy != 0 {
            self.update_search();
        }
        let Some(s) = &mut self.search else { return };
        if back {
            if s.sel.take().is_none() {
                self.search = None;
                self.mode = Mode::Browse;
            }
            return;
        }
        if let Some(i) = s.sel {
            if ok {
                let hit = &s.hits[i];
                let tab = hit.tab.clone();
                let found = !self.prefs.tabs_off.contains(&tab)
                    && self
                        .libs
                        .iter_mut()
                        .filter(|l| l.plex == hit.plex)
                        .find_map(|l| l.pages.get_mut(&tab))
                        .is_some_and(|p| p.focus_id(&hit.id));
                if found {
                    self.tab = tab;
                    self.search_f = false;
                    self.mode = Mode::Browse;
                    self.open_title();
                    if self.detail.is_none() {
                        self.return_to_search();
                    }
                } else {
                    s.invalidate();
                }
                return;
            }
            if dx != 0 || dy != 0 {
                s.sel = grid_step(i, s.hits.len(), dx, dy);
                if let Some(i) = s.sel {
                    let row = i / COLS * COLS;
                    s.first = s.first.clamp(row.saturating_sub(COLS), row);
                }
                return;
            }
        }
        if dx > 0 && s.keyboard.at_right_edge() && !s.hits.is_empty() {
            s.sel = Some(s.first);
            return;
        }
        match s.keyboard.key(&self.g, sym, ok, false, dx, dy) {
            Typed::Done(_) if !s.hits.is_empty() => s.sel = Some(s.first),
            _ if s.keyboard.text().trim() != s.query => s.invalidate(),
            _ => {}
        }
    }

    pub(super) fn return_to_search(&mut self) {
        if let Some(s) = &self.search {
            self.tab = if self.tabs().iter().any(|(tab, _)| *tab == s.from_tab) {
                s.from_tab.clone()
            } else {
                self.landing_tab()
            };
            self.search_f = true;
            self.mode = Mode::Search;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{grid_step, matches};

    #[test]
    fn search_merges_only_shared_ids_and_keeps_the_best_match() {
        let titles = [
            ("jf", "1", "The Arrival", Some("tmdbm1"), 0),
            ("plex2", "1", "Arrival", Some("tmdbm1"), 1),
            ("plex1", "1", "Arrival", Some("tmdbtv1"), 2),
            ("jf", "2", "Arrival Day", None, 3),
            ("jf", "3", "Un été", None, 4),
            ("jf", "4", "Arrival", None, 5),
            ("plex1", "5", "Arrival", None, 6),
        ];
        assert_eq!(matches("aRRIVAL", titles.into_iter()), [5, 2, 6, 1, 3]);
        assert_eq!(matches("the arrival", titles.into_iter()), [0]);
        assert_eq!(matches("ÉTÉ", titles.into_iter()), [4]);
        assert!(matches("", titles.into_iter()).is_empty());
    }

    #[test]
    fn search_grid_moves_between_rows_and_back_to_the_keyboard() {
        assert_eq!(grid_step(0, 10, -1, 0), None);
        assert_eq!(grid_step(3, 10, 1, 0), Some(3));
        assert_eq!(grid_step(3, 10, 0, 1), Some(7));
        assert_eq!(grid_step(7, 10, 0, 1), Some(9));
        assert_eq!(grid_step(9, 10, 0, -1), Some(5));
        assert_eq!(grid_step(8, 10, 0, 1), Some(8));
    }
}
