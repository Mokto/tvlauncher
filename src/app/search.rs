use super::*;

pub(super) struct Search {
    from_tab: String,
    query: String,
    pub(super) menu: Menu,
}

fn matches<'a>(
    query: &str,
    titles: impl Iterator<Item = (&'a str, &'a str, &'a str)>,
) -> Vec<(&'a str, &'a str, &'a str)> {
    let query = query.to_lowercase();
    if query.is_empty() {
        return vec![];
    }
    let mut hits: Vec<_> = titles
        .filter_map(|hit @ (_, _, name)| {
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
            Some(((rank, name, hit.0, hit.1), hit))
        })
        .collect();
    hits.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    hits.into_iter().map(|(_, hit)| hit).collect()
}

impl App {
    pub(super) fn can_search(&self) -> bool {
        self.libs
            .iter()
            .any(|lib| lib.views.iter().any(|v| !self.prefs.tabs_off.contains(&v.id)))
    }

    pub(super) fn ask_search(&mut self) {
        let query = self.search.as_ref().map_or_else(String::new, |s| s.query.clone());
        self.ask(
            Ask::Search,
            "Search",
            "Find a title in your saved libraries.",
            "Title",
            &query,
        );
    }

    pub(super) fn search_results(&mut self, query: String) {
        let tabs = self.tabs();
        let hits = matches(
            &query,
            tabs.iter().flat_map(|(tab, _)| {
                self.libs
                    .iter()
                    .filter_map(move |lib| lib.pages.get(tab))
                    .flat_map(move |p| {
                        p.titles()
                            .iter()
                            .map(move |t| (tab.as_str(), t.id.as_str(), t.name.as_str()))
                    })
            }),
        );
        // ponytail: cap eager menu textures at 32 results; render only the visible rows if the cap becomes a problem.
        const MAX_RESULTS: usize = 32;
        let label = match (query.is_empty(), hits.len()) {
            (true, _) => "Search · Enter a title".into(),
            (_, 0) => format!("No matches for “{}”", jellyfin::clip(&query, 48)),
            (_, n) if n > MAX_RESULTS => format!("Showing {MAX_RESULTS} of {n} matches · Narrow your search"),
            (_, n) => format!("Search · {n} {}", if n == 1 { "match" } else { "matches" }),
        };
        let edit = if query.is_empty() {
            "Enter a title".into()
        } else {
            format!("Edit search: {}", jellyfin::clip(&query, 48))
        };
        let mut items = vec![(Opt::SearchEdit, edit)];
        for (tab, id, name) in hits.into_iter().take(MAX_RESULTS) {
            let Some((lib, view)) = self
                .libs
                .iter()
                .find_map(|l| l.views.iter().find(|v| v.id == tab).map(|v| (l, v)))
            else {
                continue;
            };
            let source = if lib.plex { "Plex" } else { "Jellyfin" };
            let library = tabs
                .iter()
                .find(|(key, _)| key == tab)
                .map_or(view.name.as_str(), |(_, n)| n);
            let library = if view.server_name.is_empty() {
                library.to_string()
            } else {
                format!("{library} / {}", view.server_name)
            };
            items.push((
                Opt::SearchHit(tab.into(), id.into()),
                format!(
                    "{}   ·   {source} · {}",
                    jellyfin::clip(name, 45),
                    jellyfin::clip(&library, 28)
                ),
            ));
        }
        let sel = usize::from(items.len() > 1);
        let from_tab = self
            .search
            .as_ref()
            .map_or_else(|| self.tab.clone(), |s| s.from_tab.clone());
        self.detail = None;
        self.detail_back.clear();
        self.show_menu(&label, items, sel);
        if let Mode::Menu(menu) = std::mem::replace(&mut self.mode, Mode::Search) {
            self.search = Some(Search { from_tab, query, menu });
        }
    }

    pub(super) fn search_key(&mut self, dy: i32, ok: bool, back: bool) {
        if back {
            self.search = None;
            self.mode = Mode::Browse;
            self.search_f = self.can_search();
            return;
        }
        let Some(s) = &mut self.search else { return };
        if !ok {
            s.menu.sel = (s.menu.sel as i32 + dy).clamp(0, s.menu.opts.len() as i32 - 1) as usize;
            return;
        }
        match &s.menu.opts[s.menu.sel].0 {
            Opt::SearchEdit => self.ask_search(),
            Opt::SearchHit(tab, id) => {
                let tab = tab.clone();
                let found = !self.prefs.tabs_off.contains(&tab)
                    && self
                        .libs
                        .iter_mut()
                        .find_map(|l| l.pages.get_mut(&tab))
                        .is_some_and(|p| p.focus_id(id));
                if !found {
                    let query = s.query.clone();
                    return self.search_results(query);
                }
                self.tab = tab;
                self.search_f = false;
                self.detail_back.clear();
                self.mode = Mode::Browse;
                self.open_title();
                if self.detail.is_none() {
                    self.return_to_search();
                }
            }
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
            self.search_f = self.can_search();
            self.mode = Mode::Search;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn search_ranks_exact_prefix_then_substring_and_keeps_source_keys() {
        let titles = [
            ("jellyfin", "1", "The Arrival"),
            ("plex2", "1", "Arrival"),
            ("plex1", "1", "Arrival"),
            ("jellyfin", "2", "Arrival Day"),
            ("plex1", "2", "Departure"),
        ];
        assert_eq!(
            matches("aRRIVAL", titles.into_iter()),
            [titles[2], titles[1], titles[3], titles[0]]
        );
        assert_eq!(matches("ÉTÉ", [("jellyfin", "3", "Un été")].into_iter()).len(), 1);
        assert!(matches("", titles.into_iter()).is_empty());
    }
}
