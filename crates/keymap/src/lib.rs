//! The app's keyboard map (T19): one table of bindings, one pure resolver.
//!
//! The WASM client cannot host unit tests, so the mapping from "this key, on this
//! screen, while typing" to an action lives here and is covered on the host. The
//! client's single `keydown` listener only translates a DOM event into [`KeyPress`]
//! and asks [`resolve`]; the `?` overlay renders the same [`BINDINGS`] table, so the
//! help you read is the help that runs.

/// The surface a key press lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Home,
    TagDetail,
    Capture,
    Settings,
}

impl Screen {
    /// The screen a client-side path belongs to; unknown paths belong to a tag.
    pub fn from_path(path: &str) -> Screen {
        match path.trim_end_matches('/') {
            "" | "/index.html" => Screen::Home,
            "/capture" => Screen::Capture,
            "/settings" => Screen::Settings,
            _ => Screen::TagDetail,
        }
    }
}

/// An open sheet that owns keys while it is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    Search,
    Help,
}

/// The keys the map names, parsed from `KeyboardEvent.key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Slash,
    Comma,
    Question,
    Escape,
    Enter,
    Tab,
    A,
    C,
    D,
    H,
    K,
    X,
}

impl Key {
    /// Parse the printable `KeyboardEvent.key` values this map answers to.
    pub fn from_dom(raw: &str) -> Option<Key> {
        match raw {
            "/" => Some(Key::Slash),
            "?" => Some(Key::Question),
            "Escape" => Some(Key::Escape),
            "," | "<" => Some(Key::Comma),
            "Enter" => Some(Key::Enter),
            "Tab" => Some(Key::Tab),
            letter if letter.len() == 1 => match letter.to_ascii_lowercase().as_str() {
                "a" => Some(Key::A),
                "c" => Some(Key::C),
                "d" => Some(Key::D),
                "h" => Some(Key::H),
                "k" => Some(Key::K),
                "x" => Some(Key::X),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The parts of a `keydown` the map reacts to; `typing` is true while a field has focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPress {
    pub key: Key,
    pub shift: bool,
    pub ctrl: bool,
    pub meta: bool,
    pub alt: bool,
    pub typing: bool,
}

impl KeyPress {
    /// A plain press of `key`, handy for tests and for the listener's defaults.
    pub fn plain(key: Key) -> KeyPress {
        KeyPress {
            key,
            shift: false,
            ctrl: false,
            meta: false,
            alt: false,
            typing: false,
        }
    }

    /// True when the platform's command modifier is held (Control or Command).
    fn chord(self) -> bool {
        self.ctrl || self.meta
    }

    /// True when no modifier at all is held, so a plain letter is a plain letter.
    fn plain_press(self) -> bool {
        !self.ctrl && !self.meta && !self.alt
    }
}

/// What a mapped key asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    OpenSearch,
    CloseSearch,
    OpenHelp,
    CloseHelp,
    OpenCapture,
    Back,
    ArchiveThread,
    ApproveSummary,
    DiscardSummary,
    ToggleHistory,
    OpenSettings,
}

/// Where a binding applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Anywhere,
    /// The app plus the map itself, so the key that opens the map also shuts it.
    AppAndMap,
    Screen(Screen),
    Search,
    Help,
}

impl Scope {
    /// True when this scope is in play for the current screen and overlay.
    fn live(self, screen: Screen, overlay: Option<Overlay>) -> bool {
        match (self, overlay) {
            (Scope::Anywhere, None) => true,
            (Scope::AppAndMap, None | Some(Overlay::Help)) => true,
            (Scope::Screen(only), None) => only == screen,
            (Scope::Search, Some(Overlay::Search)) => true,
            (Scope::Help, Some(Overlay::Help)) => true,
            _ => false,
        }
    }
}

/// One row of the documented map; `?` prints these, `resolve` reads them.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    /// How the chord reads in the help sheet.
    pub keys: &'static str,
    /// What it does, in one line.
    pub label: &'static str,
    /// The gesture a phone gets instead of the keys.
    pub touch: &'static str,
    pub section: Section,
    pub scope: Scope,
    /// `None` for keys the browser or the focused element already handles.
    pub key: Option<Key>,
    /// True when the binding needs Control or Command.
    pub chord: bool,
    pub action: Option<Action>,
}

/// The help sheet's groups, in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Moving,
    Search,
    Review,
    Capture,
    Account,
}

impl Section {
    /// The heading this group prints.
    pub fn heading(self) -> &'static str {
        match self {
            Section::Moving => "Moving around",
            Section::Search => "Search and help",
            Section::Review => "Reviewing a tag",
            Section::Capture => "Capturing",
            Section::Account => "Your account",
        }
    }
}

/// Every shortcut the app has, including the ones the DOM handles for us.
pub const BINDINGS: &[Binding] = &[
    Binding {
        keys: "Tab / Shift+Tab",
        label: "Move focus in visual order",
        touch: "Tap in order",
        section: Section::Moving,
        scope: Scope::Anywhere,
        key: Some(Key::Tab),
        chord: false,
        action: None,
    },
    Binding {
        keys: "Enter",
        label: "Open the focused tag, or fold a thread open",
        touch: "Tap the card",
        section: Section::Moving,
        scope: Scope::Anywhere,
        key: Some(Key::Enter),
        chord: false,
        action: None,
    },
    Binding {
        keys: "Esc",
        label: "Back to the grid",
        touch: "Tap Back",
        section: Section::Moving,
        scope: Scope::Screen(Screen::TagDetail),
        key: Some(Key::Escape),
        chord: false,
        action: Some(Action::Back),
    },
    Binding {
        keys: "Alt + Left",
        label: "Back (browser history)",
        touch: "Swipe from edge",
        section: Section::Moving,
        scope: Scope::Anywhere,
        key: None,
        chord: false,
        action: None,
    },
    Binding {
        keys: "/",
        label: "Search notes",
        touch: "Pull down on the grid",
        section: Section::Search,
        scope: Scope::Anywhere,
        key: Some(Key::Slash),
        chord: false,
        action: Some(Action::OpenSearch),
    },
    Binding {
        keys: "Ctrl K / Cmd K",
        label: "Search notes",
        touch: "Pull down on the grid",
        section: Section::Search,
        scope: Scope::Anywhere,
        key: Some(Key::K),
        chord: true,
        action: Some(Action::OpenSearch),
    },
    Binding {
        keys: "Esc",
        label: "Close search",
        touch: "Swipe up, or tap outside",
        section: Section::Search,
        scope: Scope::Search,
        key: Some(Key::Escape),
        chord: false,
        action: Some(Action::CloseSearch),
    },
    Binding {
        keys: "?",
        label: "Show or close this map",
        touch: "Tap the ? button",
        section: Section::Search,
        scope: Scope::AppAndMap,
        key: Some(Key::Question),
        chord: false,
        action: Some(Action::OpenHelp),
    },
    Binding {
        keys: "Esc",
        label: "Close this map",
        touch: "Tap the close button",
        section: Section::Search,
        scope: Scope::Help,
        key: Some(Key::Escape),
        chord: false,
        action: Some(Action::CloseHelp),
    },
    Binding {
        keys: "A",
        label: "Approve the pending summary",
        touch: "Tap Approve",
        section: Section::Review,
        scope: Scope::Screen(Screen::TagDetail),
        key: Some(Key::A),
        chord: false,
        action: Some(Action::ApproveSummary),
    },
    Binding {
        keys: "D",
        label: "Discard the pending summary",
        touch: "Tap Discard",
        section: Section::Review,
        scope: Scope::Screen(Screen::TagDetail),
        key: Some(Key::D),
        chord: false,
        action: Some(Action::DiscardSummary),
    },
    Binding {
        keys: "H",
        label: "Show or hide summary versions",
        touch: "Tap Versions",
        section: Section::Review,
        scope: Scope::Screen(Screen::TagDetail),
        key: Some(Key::H),
        chord: false,
        action: Some(Action::ToggleHistory),
    },
    Binding {
        keys: "X",
        label: "Archive the focused thread",
        touch: "Tap Archive",
        section: Section::Review,
        scope: Scope::Screen(Screen::TagDetail),
        key: Some(Key::X),
        chord: false,
        action: Some(Action::ArchiveThread),
    },
    Binding {
        keys: "C",
        label: "Quick capture a note",
        touch: "Tap Capture",
        section: Section::Capture,
        scope: Scope::Anywhere,
        key: Some(Key::C),
        chord: false,
        action: Some(Action::OpenCapture),
    },
    Binding {
        keys: ",",
        label: "Open settings",
        touch: "Tap Account",
        section: Section::Account,
        scope: Scope::Anywhere,
        key: Some(Key::Comma),
        chord: false,
        action: Some(Action::OpenSettings),
    },
];

/// The action a press asks for, if the map claims it.
///
/// Overlays own their keys, plain letters are ignored while a field has focus, and
/// holding Control, Command or Alt turns every plain binding off.
pub fn resolve(press: KeyPress, screen: Screen, overlay: Option<Overlay>) -> Option<Action> {
    BINDINGS.iter().find_map(|binding| {
        let key = binding.key?;
        let action = binding.action?;
        if key != press.key || !binding.scope.live(screen, overlay) {
            return None;
        }
        if binding.chord {
            // Chords are the one thing that still works from inside a field.
            return (press.chord() && !press.alt).then_some(action);
        }
        if !press.plain_press() {
            return None;
        }
        // A field keeps its letters; only the sheet around it answers to Escape.
        if press.typing && overlay.is_none() {
            return None;
        }
        // `?` is a toggle: inside the map it closes what it opened.
        if action == Action::OpenHelp && overlay == Some(Overlay::Help) {
            return Some(Action::CloseHelp);
        }
        Some(action)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(key: Key) -> KeyPress {
        KeyPress {
            typing: true,
            ..KeyPress::plain(key)
        }
    }

    fn with(key: Key, ctrl: bool, meta: bool, alt: bool) -> KeyPress {
        KeyPress {
            ctrl,
            meta,
            alt,
            ..KeyPress::plain(key)
        }
    }

    #[test]
    fn comma_opens_settings_but_not_from_a_field() {
        assert_eq!(
            resolve(KeyPress::plain(Key::Comma), Screen::Home, None),
            Some(Action::OpenSettings)
        );
        assert_eq!(resolve(typed(Key::Comma), Screen::Home, None), None);
    }

    #[test]
    fn every_row_names_a_touch_gesture() {
        for binding in BINDINGS {
            assert!(
                !binding.touch.trim().is_empty(),
                "{} has no gesture",
                binding.keys
            );
        }
    }

    #[test]
    fn plain_letters_are_ignored_while_typing() {
        for key in [
            Key::A,
            Key::C,
            Key::D,
            Key::H,
            Key::X,
            Key::Slash,
            Key::Question,
        ] {
            assert_eq!(
                resolve(typed(key), Screen::TagDetail, None),
                None,
                "{key:?}"
            );
        }
    }

    #[test]
    fn the_search_sheet_still_closes_from_its_own_field() {
        assert_eq!(
            resolve(typed(Key::Escape), Screen::Home, Some(Overlay::Search)),
            Some(Action::CloseSearch)
        );
    }

    #[test]
    fn chords_survive_a_focused_field() {
        assert_eq!(
            resolve(typed(Key::K), Screen::Home, None),
            None,
            "letter without a modifier is text"
        );
        assert_eq!(
            resolve(with(Key::K, true, false, false), Screen::TagDetail, None),
            Some(Action::OpenSearch)
        );
        assert_eq!(
            resolve(
                KeyPress {
                    typing: true,
                    ..with(Key::K, false, true, false)
                },
                Screen::Capture,
                None
            ),
            Some(Action::OpenSearch)
        );
    }

    #[test]
    fn held_modifiers_turn_plain_bindings_off() {
        for press in [
            with(Key::A, true, false, false),
            with(Key::D, false, true, false),
            with(Key::X, false, false, true),
            with(Key::Slash, true, false, false),
            with(Key::Question, false, true, false),
        ] {
            assert_eq!(resolve(press, Screen::TagDetail, None), None, "{press:?}");
        }
    }

    #[test]
    fn shift_alone_still_reaches_the_binding() {
        let press = KeyPress {
            shift: true,
            ..KeyPress::plain(Key::Question)
        };
        assert_eq!(resolve(press, Screen::Home, None), Some(Action::OpenHelp));
    }

    #[test]
    fn review_keys_only_answer_on_a_tag() {
        for key in [Key::A, Key::D, Key::H, Key::X] {
            assert_eq!(
                resolve(KeyPress::plain(key), Screen::Home, None),
                None,
                "{key:?}"
            );
            assert_eq!(
                resolve(KeyPress::plain(key), Screen::Settings, None),
                None,
                "{key:?}"
            );
        }
        assert_eq!(
            resolve(KeyPress::plain(Key::A), Screen::TagDetail, None),
            Some(Action::ApproveSummary)
        );
        assert_eq!(
            resolve(KeyPress::plain(Key::X), Screen::TagDetail, None),
            Some(Action::ArchiveThread)
        );
    }

    #[test]
    fn escape_goes_back_only_from_a_tag() {
        assert_eq!(
            resolve(KeyPress::plain(Key::Escape), Screen::TagDetail, None),
            Some(Action::Back)
        );
        assert_eq!(
            resolve(KeyPress::plain(Key::Escape), Screen::Home, None),
            None
        );
    }

    #[test]
    fn the_map_and_search_reach_every_screen() {
        for screen in [
            Screen::Home,
            Screen::TagDetail,
            Screen::Capture,
            Screen::Settings,
        ] {
            assert_eq!(
                resolve(KeyPress::plain(Key::Question), screen, None),
                Some(Action::OpenHelp),
                "{screen:?}"
            );
            assert_eq!(
                resolve(KeyPress::plain(Key::Slash), screen, None),
                Some(Action::OpenSearch),
                "{screen:?}"
            );
            assert_eq!(
                resolve(KeyPress::plain(Key::C), screen, None),
                Some(Action::OpenCapture),
                "{screen:?}"
            );
        }
    }

    #[test]
    fn the_help_sheet_is_closed_by_escape_or_its_own_key() {
        assert_eq!(
            resolve(
                KeyPress::plain(Key::Escape),
                Screen::Home,
                Some(Overlay::Help)
            ),
            Some(Action::CloseHelp)
        );
        assert_eq!(
            resolve(
                KeyPress::plain(Key::Question),
                Screen::Home,
                Some(Overlay::Help)
            ),
            Some(Action::CloseHelp)
        );
        assert_eq!(
            resolve(
                KeyPress::plain(Key::Slash),
                Screen::Home,
                Some(Overlay::Help)
            ),
            None,
            "the map does not stack another sheet on top"
        );
    }

    #[test]
    fn an_open_sheet_swallows_screen_keys() {
        assert_eq!(
            resolve(
                KeyPress::plain(Key::A),
                Screen::TagDetail,
                Some(Overlay::Search)
            ),
            None
        );
        assert_eq!(
            resolve(
                KeyPress::plain(Key::Escape),
                Screen::TagDetail,
                Some(Overlay::Search)
            ),
            Some(Action::CloseSearch),
            "Escape closes the sheet before it leaves the tag"
        );
    }

    #[test]
    fn paths_pick_the_screen() {
        assert_eq!(Screen::from_path("/"), Screen::Home);
        assert_eq!(Screen::from_path("/capture"), Screen::Capture);
        assert_eq!(Screen::from_path("/capture/"), Screen::Capture);
        assert_eq!(Screen::from_path("/settings"), Screen::Settings);
        assert_eq!(
            Screen::from_path("/tags/0192f0aa-1111-7000-8000-000000000000"),
            Screen::TagDetail
        );
    }

    #[test]
    fn keys_parse_the_way_browsers_report_them() {
        assert_eq!(Key::from_dom("Escape"), Some(Key::Escape));
        assert_eq!(Key::from_dom("?"), Some(Key::Question));
        assert_eq!(Key::from_dom("A"), Some(Key::A));
        assert_eq!(Key::from_dom("x"), Some(Key::X));
        assert_eq!(Key::from_dom("q"), None);
        assert_eq!(Key::from_dom("Shift"), None);
    }

    #[test]
    fn the_ticket_actions_are_documented() {
        let documented: Vec<Action> = BINDINGS
            .iter()
            .filter_map(|binding| binding.action)
            .collect();
        for action in [
            Action::OpenSearch,
            Action::Back,
            Action::ArchiveThread,
            Action::OpenCapture,
            Action::ApproveSummary,
            Action::DiscardSummary,
            Action::ToggleHistory,
            Action::OpenHelp,
        ] {
            assert!(
                documented.contains(&action),
                "{action:?} missing from the map"
            );
        }
    }

    #[test]
    fn every_binding_that_acts_can_be_matched() {
        for binding in BINDINGS {
            assert!(!binding.label.is_empty(), "{} needs a label", binding.keys);
            if binding.action.is_some() {
                assert!(
                    binding.key.is_some(),
                    "{} acts but names no key",
                    binding.keys
                );
            }
            if binding.key.is_none() {
                assert!(
                    binding.action.is_none(),
                    "{} is documentation only",
                    binding.keys
                );
            }
        }
    }
}
