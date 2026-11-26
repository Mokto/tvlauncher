use super::*;

#[derive(Clone, Copy)]
enum Target {
    Profile,
    Picture(usize),
}

struct Card {
    avatar: usize,
    name: Option<Tex>,
}

pub(super) struct Picker {
    target: Target,
    title: Option<Tex>,
    note: Option<Tex>,
    help: Option<Tex>,
    cards: Vec<Card>,
    selected: usize,
}

const COLS: usize = 7;
const VISIBLE_ROWS: usize = 2;

fn move_card(i: usize, count: usize, dx: i32, dy: i32) -> usize {
    let row = i / COLS;
    let first = row * COLS;
    if dy < 0 && row > 0 {
        return i - COLS;
    }
    if dy > 0 && first + COLS < count {
        return (i + COLS).min(count - 1);
    }
    i.saturating_add_signed(dx as isize)
        .clamp(first, (first + COLS).min(count) - 1)
}

impl Picker {
    fn new(g: &Gfx, target: Target, cards: Vec<Card>, selected: usize, note: &str) -> Self {
        let (title, help) = match target {
            Target::Profile => ("Who’s watching?", "Choose a profile and press OK"),
            Target::Picture(_) => ("Choose a profile picture", "OK to use this picture   Back to cancel"),
        };
        Self {
            target,
            title: g.text(g.f_hero, title, 0),
            note: (!note.is_empty()).then(|| g.text(g.f_sub, note, 0)).flatten(),
            help: g.text(g.f_label, help, 0),
            cards,
            selected,
        }
    }

    pub(super) fn draw(&self, g: &Gfx) {
        let center = |t: Option<&Tex>, x: f32, y, color| {
            g.put(t, x - t.map_or(0.0, |t| t.w as f32 / 2.0), y, color, 1.0);
        };
        avatars::backdrop(g);
        let pictures = matches!(self.target, Target::Picture(_));
        let many = self.cards.len() > COLS;
        let title_y = if many { 88.0 } else { 120.0 };
        center(self.title.as_ref(), W / 2.0, title_y, TEXT);
        center(self.note.as_ref(), W / 2.0, title_y + 105.0, YELLOW);
        center(self.help.as_ref(), W / 2.0, 1000.0, DIM);
        let visible_rows = if pictures { 3 } else { VISIBLE_ROWS };
        let first_row = (self.selected / COLS).saturating_sub(visible_rows - 1);
        let size = if pictures { 168.0 } else { 220.0 };
        for (row, cards) in self.cards.chunks(COLS).enumerate().skip(first_row).take(visible_rows) {
            let step = if pictures {
                240.0
            } else if cards.len() == COLS {
                252.0
            } else {
                292.0
            };
            let cols = if pictures { COLS } else { cards.len() };
            let margin = (W - (cols - 1) as f32 * step - size) / 2.0;
            let y = if pictures {
                230.0 + (row - first_row) as f32 * 245.0
            } else if many {
                262.0 + (row - first_row) as f32 * 344.0
            } else {
                390.0
            };
            for (col, card) in cards.iter().enumerate() {
                let x = margin + col as f32 * step;
                let on = row * COLS + col == self.selected;
                if on {
                    g.rounded(
                        x - 7.0,
                        y - 7.0,
                        size + 14.0,
                        size + 14.0,
                        (size + 14.0) / 2.0,
                        WHITE,
                        1.0,
                    );
                }
                avatars::draw(g, card.avatar, x, y, size);
                let name_y = y + size + 20.0;
                center(
                    card.name.as_ref(),
                    x + size / 2.0,
                    name_y,
                    if on { WHITE } else { TEXT },
                );
            }
        }
    }
}

impl App {
    pub(super) fn choose_profile(&mut self, i: usize, note: &str) {
        let cards = self
            .profiles
            .iter()
            .map(|p| Card {
                avatar: p.avatar,
                name: self.g.text(self.g.f_row, &p.name, 222),
            })
            .collect();
        self.mode = Mode::Picker(Box::new(Picker::new(&self.g, Target::Profile, cards, i, note)));
    }

    pub(super) fn switch_profiles(&mut self) {
        let Some(selected) = self.active else { return };
        self.reload_profile(None, None);
        self.switch_from = Some(selected);
        self.choose_profile(selected, "");
    }

    fn choose_picture(&mut self, i: usize) {
        let cards = avatars::NAMES
            .iter()
            .enumerate()
            .map(|(avatar, name)| Card {
                avatar,
                name: self.g.text(self.g.f_row, name, 222),
            })
            .collect();
        let selected = self.profiles[i].avatar;
        self.mode = Mode::Picker(Box::new(Picker::new(&self.g, Target::Picture(i), cards, selected, "")));
    }

    pub(super) fn picker_key(&mut self, dx: i32, dy: i32, ok: bool, back: bool) {
        let editable = self.manager() && self.editing.is_none();
        let Mode::Picker(p) = &mut self.mode else { return };
        let selected = p.selected;
        match p.target {
            Target::Profile if back => {
                if let Some(i) = self.switch_from.take() {
                    self.reload_profile(Some(i), None);
                }
            }
            Target::Profile if ok => self.select_profile(selected),
            Target::Picture(i) if back => self.manage_profile(i),
            Target::Picture(i) if ok && editable => {
                let mut list = self.profiles.clone();
                list[i].avatar = selected;
                if self.update_profiles(list) {
                    self.manage_profile(i);
                }
            }
            _ => p.selected = move_card(selected, p.cards.len(), dx, dy),
        }
    }

    fn select_profile(&mut self, i: usize) {
        if self.active.is_some() {
            return;
        }
        if self.profiles[i].pin.is_empty() {
            self.reload_profile(Some(i), None);
        } else if self.pin_retry.is_some_and(|until| until > Instant::now()) {
            self.choose_profile(i, "Try again in a few seconds");
        } else {
            self.ask(Ask::Unlock(i), "", &self.profiles[i].name.clone(), "PIN", "");
        }
    }

    pub(super) fn profile_entries(&self) -> Vec<Entry> {
        std::iter::once(Entry {
            act: Act::AddProfile,
            title: "Add profile".into(),
            desc: "Create a Manager, Adult or Kid profile.".into(),
            value: Value::Text("Add".into()),
        })
        .chain(self.profiles.iter().enumerate().map(|(i, p)| {
            Entry {
                act: Act::Profile(i),
                title: p.name.clone(),
                desc: if Some(i) == self.active {
                    "Current profile"
                } else if p.pin.is_empty() {
                    "No PIN"
                } else {
                    "PIN protected"
                }
                .into(),
                value: Value::Text(p.role.label().into()),
            }
        }))
        .collect()
    }

    pub(super) fn add_profile(&mut self) {
        self.show_menu(
            "Choose a role",
            Role::ALL
                .into_iter()
                .map(|r| (Opt::NewProfile(r), r.label().into()))
                .collect(),
            0,
        );
    }

    pub(super) fn manage_profile(&mut self, i: usize) {
        let p = &self.profiles[i];
        let title = format!("{} · {}", p.name, p.role.label());
        let mut list = vec![
            (Opt::ProfileMedia(i), "Jellyfin, Plex and Tabs".into()),
            (Opt::RenameProfile(i), "Rename".into()),
            (
                Opt::ProfilePin(i),
                if p.pin.is_empty() { "Set PIN" } else { "Change PIN" }.into(),
            ),
        ];
        if p.role == Role::Kid {
            list.push((Opt::ProfileApps(i), format!("Allowed apps ({})", p.apps.len())));
        }
        list.push((Opt::ProfilePicture(i), "Profile picture".into()));
        if !p.pin.is_empty() {
            list.push((Opt::RemovePin(i), "Remove PIN".into()));
        }
        self.show_menu(&title, list, 0);
    }

    fn profile_apps(&mut self, i: usize, selected: Option<&str>) {
        let p = &self.profiles[i];
        let mut list: Vec<_> = self
            .all_apps()
            .map(|app| {
                let state = if p.allows(&app.id) { "Allowed" } else { "Blocked" };
                (
                    Opt::ToggleApp(i, app.id.clone()),
                    format!("{} · {state}", jellyfin::clip(&app.title, 35)),
                )
            })
            .collect();
        list.sort_by(|a, b| a.1.cmp(&b.1));
        let sel = selected
            .and_then(|id| {
                list.iter()
                    .position(|(o, _)| matches!(o, Opt::ToggleApp(_, app) if app == id))
            })
            .unwrap_or(0);
        if list.is_empty() {
            list.push((Opt::Close, "No apps found".into()));
        }
        self.show_menu("Allowed apps · OK to toggle", list, sel);
    }

    fn update_profiles(&mut self, list: Vec<Profile>) -> bool {
        match profiles::save(&self.luna.root, &list) {
            Ok(()) => {
                self.profiles = list;
                self.profile_label = self.active.filter(|_| self.profiles.len() > 1).and_then(|i| {
                    self.g
                        .text(self.g.f_label, &jellyfin::clip(&self.profiles[i].name, 13), 0)
                });
                true
            }
            Err(e) => self.profile_save_error(e),
        }
    }

    fn profile_save_error(&mut self, e: std::io::Error) -> bool {
        log(&format!("profiles: {e}"));
        self.show_menu("Could not save the profile", vec![(Opt::Close, "OK".into())], 0);
        false
    }

    pub(super) fn account_option(&mut self, opt: &Opt) {
        if !self.manager() || self.editing.is_some() {
            return;
        }
        match *opt {
            Opt::NewProfile(r) => self.ask(Ask::NewProfile(r), "New profile", r.label(), "Name", ""),
            Opt::RenameProfile(i) => self.ask(
                Ask::ProfileName(i),
                "Rename profile",
                "Choose a name.",
                "Name",
                &self.profiles[i].name.clone(),
            ),
            Opt::ProfilePin(i) => self.ask(
                Ask::ProfilePin(i),
                "Profile PIN",
                "Choose a 4-digit PIN.",
                "New PIN",
                "",
            ),
            Opt::RemovePin(i) => self.show_menu(
                "Remove this profile’s PIN?",
                vec![(Opt::Close, "Cancel".into()), (Opt::ClearPin(i), "Remove PIN".into())],
                0,
            ),
            Opt::ClearPin(i) => {
                let mut list = self.profiles.clone();
                list[i].pin.clear();
                if self.update_profiles(list) {
                    self.manage_profile(i);
                }
            }
            Opt::ProfilePicture(i) => self.choose_picture(i),
            Opt::ProfileApps(i) => self.profile_apps(i, None),
            Opt::ToggleApp(i, ref id) => {
                let mut list = self.profiles.clone();
                let apps = &mut list[i].apps;
                match apps.iter().position(|a| a == id) {
                    Some(at) => drop(apps.remove(at)),
                    None => apps.push(id.clone()),
                }
                if self.update_profiles(list) {
                    self.profile_apps(i, Some(id));
                }
            }
            Opt::ProfileMedia(i) => self.reload_profile(self.active, Some(i)),
            _ => {}
        }
    }

    pub(super) fn answer_account(&mut self, ask: Ask, value: String) {
        if let Ask::Unlock(i) = ask {
            if self.active.is_some() {
                return;
            }
            if !self.profiles[i].pin.is_empty() && self.profiles[i].pin == value {
                self.reload_profile(Some(i), None);
            } else {
                self.pin_retry = Some(Instant::now() + Duration::from_secs(3));
                self.choose_profile(i, "Wrong PIN · try again in a few seconds");
            }
            return;
        }
        if !self.manager() || self.editing.is_some() {
            return;
        }
        let mut list = self.profiles.clone();
        let new = matches!(ask, Ask::NewProfile(_));
        let i = match ask {
            Ask::NewProfile(role) if profiles::valid_name(&value) => {
                let i = list.len();
                let root = profiles::data_root(&self.luna.root, i);
                if let Err(e) = Prefs::init_profile(&self.luna.root, &root) {
                    self.profile_save_error(e);
                    return;
                }
                let mut profile = Profile::new(value, role);
                profile.avatar = i % profiles::AVATAR_COUNT;
                list.push(profile);
                i
            }
            Ask::ProfileName(i) if profiles::valid_name(&value) => {
                list[i].name = value;
                i
            }
            Ask::ProfilePin(i) if value.len() == profiles::NEW_PIN_LEN && profiles::valid_pin(&value) => {
                return self.ask(
                    Ask::ConfirmPin(i, value),
                    "Confirm PIN",
                    "Enter the new PIN again.",
                    "PIN",
                    "",
                );
            }
            Ask::ConfirmPin(i, first) if first == value => {
                list[i].pin = value;
                i
            }
            Ask::ConfirmPin(i, _) => {
                return self.ask(
                    Ask::ProfilePin(i),
                    "Profile PIN",
                    "The PINs didn't match. Enter a new PIN.",
                    "New PIN",
                    "",
                );
            }
            Ask::ProfileName(i) => {
                return self.ask(
                    Ask::ProfileName(i),
                    "Rename profile",
                    "Use a name of 1–20 characters.",
                    "Name",
                    &value,
                )
            }
            Ask::NewProfile(r) => {
                return self.ask(
                    Ask::NewProfile(r),
                    "New profile",
                    "Use a name of 1–20 characters.",
                    "Name",
                    &value,
                )
            }
            _ => return,
        };
        if self.update_profiles(list) {
            if new {
                self.choose_picture(i);
            } else {
                self.manage_profile(i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::move_card;

    #[test]
    fn profile_grid_does_not_wrap_or_select_missing_cards() {
        assert_eq!(move_card(6, 9, 1, 0), 6);
        assert_eq!(move_card(6, 9, 0, 1), 8);
        assert_eq!(move_card(7, 9, -1, 0), 7);
        assert_eq!(move_card(8, 9, 0, 1), 8);
        assert_eq!(move_card(8, 9, 0, -1), 1);
        assert_eq!(move_card(0, 1, 1, 0), 0);
    }
}
