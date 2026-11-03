//! The keys the app handles, in one table. The ☰ menu, the tooltips that mention a key and
//! the Keyboard shortcuts window read their text from it, and the handlers ask [`take`] for
//! their [`Command`], so the list cannot drift from what the keys do.

use eframe::egui::{self, Context, Event, Key, Modifiers};

/// What a key asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    OpenMap,
    Panel,
    Search,
    Print,
    Shortcuts,
    DebugOverlay,
    Pan,
    ZoomIn,
    ZoomOut,
    Close,
}

/// One way of pressing a key.
#[derive(Clone, Copy, Debug)]
pub enum Binding {
    /// A key with its modifiers.
    Key(Modifiers, Key),
    /// A character as typed, whatever the keyboard layout puts it on.
    Text(&'static str),
}

use Binding::{Key as K, Text};

const CTRL: Modifiers = Modifiers::COMMAND;
const NONE: Modifiers = Modifiers::NONE;

/// The arrow keys, in the order left, right, up, down.
pub const ARROWS: [Key; 4] = [
    Key::ArrowLeft,
    Key::ArrowRight,
    Key::ArrowUp,
    Key::ArrowDown,
];

pub struct Entry {
    pub command: Command,
    /// What the key does, for the list.
    pub what: &'static str,
    /// The ways to press it; the first is the one shown in tooltips.
    pub keys: &'static [Binding],
    /// Whether it works while a text field has the keys.
    pub typing: bool,
}

/// Every key the app handles. The keys inside the search box and the results (↑ ↓ Enter) are
/// in [`LIST_KEYS`].
pub const TABLE: &[Entry] = &[
    Entry {
        command: Command::OpenMap,
        what: "Open a map",
        keys: &[K(CTRL, Key::O)],
        typing: true,
    },
    Entry {
        command: Command::Panel,
        what: "Show or hide the panel",
        keys: &[K(CTRL, Key::B)],
        typing: true,
    },
    Entry {
        command: Command::Search,
        what: "Search",
        keys: &[K(CTRL, Key::F), Text("/")],
        typing: false,
    },
    Entry {
        command: Command::Print,
        what: "Print the map",
        keys: &[K(CTRL, Key::P)],
        typing: true,
    },
    Entry {
        command: Command::Shortcuts,
        what: "Show this list of keys",
        keys: &[K(NONE, Key::F1)],
        typing: true,
    },
    Entry {
        command: Command::DebugOverlay,
        what: "Show or hide the debug overlay",
        keys: &[K(NONE, Key::F3)],
        typing: true,
    },
    Entry {
        command: Command::Pan,
        what: "Move the map",
        keys: &[
            K(NONE, Key::ArrowLeft),
            K(NONE, Key::ArrowRight),
            K(NONE, Key::ArrowUp),
            K(NONE, Key::ArrowDown),
        ],
        typing: false,
    },
    Entry {
        command: Command::ZoomIn,
        what: "Zoom in",
        keys: &[K(NONE, Key::Plus), K(NONE, Key::Equals)],
        typing: false,
    },
    Entry {
        command: Command::ZoomOut,
        what: "Zoom out",
        keys: &[K(NONE, Key::Minus)],
        typing: false,
    },
    Entry {
        command: Command::Close,
        what: "Close a window, the search results or the search marker",
        keys: &[K(NONE, Key::Escape)],
        typing: true,
    },
];

/// Keys that only work inside the search box, with what they do.
pub const LIST_KEYS: &[(&str, &str)] = &[
    ("\u{2191} \u{2193}", "Choose a search result"),
    ("Enter", "Go to the chosen result"),
];

pub fn entry(command: Command) -> &'static Entry {
    TABLE
        .iter()
        .find(|e| e.command == command)
        .expect("every command has an entry")
}

fn key_name(key: Key) -> String {
    match key {
        Key::ArrowLeft => "\u{2190}".into(),
        Key::ArrowRight => "\u{2192}".into(),
        Key::ArrowUp => "\u{2191}".into(),
        Key::ArrowDown => "\u{2193}".into(),
        Key::Minus => "\u{2212}".into(),
        Key::Escape => "Esc".into(),
        k => k.symbol_or_name().to_owned(),
    }
}

/// A binding as written for people: "Ctrl+O", "F1", "/".
pub fn text(binding: Binding) -> String {
    match binding {
        Text(t) => t.to_owned(),
        K(m, k) if m.command => format!("Ctrl+{}", key_name(k)),
        K(_, k) => key_name(k),
    }
}

/// The keys of a command as written for the list: "Ctrl+F or /"; the arrows together.
pub fn keys_text(command: Command) -> String {
    let e = entry(command);
    if command == Command::Pan {
        return e
            .keys
            .iter()
            .map(|b| text(*b))
            .collect::<Vec<_>>()
            .join(" ");
    }
    e.keys
        .iter()
        .map(|b| text(*b))
        .collect::<Vec<_>>()
        .join(" or ")
}

/// The key to mention in a tooltip or menu item: the command's first, "Ctrl+B".
pub fn hint(command: Command) -> String {
    text(entry(command).keys[0])
}

/// `label` followed by the command's key in the form the menus and tooltips use:
/// "Open a map…  (Ctrl+O)".
pub fn with_hint(label: &str, command: Command) -> String {
    format!("{label}  ({})", hint(command))
}

/// Whether one of the command's keys was pressed this frame, taking it so nothing else
/// sees it. Commands that do not work while typing return false when a text field has the
/// keys.
pub fn take(ctx: &Context, command: Command) -> bool {
    let e = entry(command);
    if !e.typing && ctx.egui_wants_keyboard_input() {
        return false;
    }
    ctx.input_mut(|i| {
        let mut hit = false;
        for b in e.keys {
            hit |= match *b {
                K(m, k) => i.consume_key(m, k),
                Text(t) => {
                    let before = i.events.len();
                    i.events
                        .retain(|ev| !matches!(ev, Event::Text(s) if s == t));
                    i.events.len() != before
                }
            };
        }
        hit
    })
}

/// Whether an arrow key is held (for the map to move while it is).
pub fn arrow_down(ctx: &egui::Context) -> bool {
    ctx.input(|i| ARROWS.into_iter().any(|k| i.key_down(k)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All the commands, as a match so a new one has to be added here.
    fn all() -> Vec<Command> {
        use Command::*;
        [
            OpenMap,
            Panel,
            Search,
            Print,
            Shortcuts,
            DebugOverlay,
            Pan,
            ZoomIn,
            ZoomOut,
            Close,
        ]
        .into_iter()
        .inspect(|c| match c {
            OpenMap | Panel | Search | Print | Shortcuts | DebugOverlay | Pan | ZoomIn
            | ZoomOut | Close => {}
        })
        .collect()
    }

    #[test]
    fn every_command_has_one_entry() {
        for c in all() {
            assert_eq!(TABLE.iter().filter(|e| e.command == c).count(), 1, "{c:?}");
        }
        assert_eq!(TABLE.len(), all().len());
    }

    #[test]
    fn no_key_means_two_things() {
        let mut seen: Vec<String> = Vec::new();
        for e in TABLE {
            for b in e.keys {
                let t = text(*b);
                assert!(!seen.contains(&t), "{t} is used twice");
                seen.push(t);
            }
        }
    }

    #[test]
    fn the_text_reads_as_keys() {
        assert_eq!(hint(Command::OpenMap), "Ctrl+O");
        assert_eq!(keys_text(Command::Search), "Ctrl+F or /");
        assert_eq!(hint(Command::Shortcuts), "F1");
        assert_eq!(hint(Command::Close), "Esc");
        assert_eq!(
            with_hint("Debug overlay", Command::DebugOverlay),
            "Debug overlay  (F3)"
        );
    }

    /// Runs one frame with `events` and says whether `command` was taken in it.
    fn pressed(command: Command, events: Vec<Event>) -> bool {
        let ctx = Context::default();
        let raw = egui::RawInput {
            events,
            focused: true,
            ..Default::default()
        };
        let mut hit = false;
        let mut full = ctx.run_ui(raw, |ui| hit = take(ui.ctx(), command));
        full.textures_delta.clear();
        hit
    }

    fn key(m: Modifiers, k: Key) -> Event {
        Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        }
    }

    #[test]
    fn each_entry_is_taken_by_its_keys() {
        for e in TABLE {
            for b in e.keys {
                let ev = match *b {
                    K(m, k) => key(m, k),
                    Text(t) => Event::Text(t.into()),
                };
                assert!(
                    pressed(e.command, vec![ev]),
                    "{:?} by {}",
                    e.command,
                    text(*b)
                );
            }
            assert!(
                !pressed(e.command, vec![Event::Text("q".into())]),
                "{:?} by q",
                e.command
            );
        }
    }

    /// The handlers are in the source files: each command is asked for outside this file
    /// (the arrows, which are held rather than pressed, by `arrow_down` and `ARROWS`).
    #[test]
    fn each_entry_has_a_handler() {
        let files = [
            include_str!("app/mod.rs"),
            include_str!("app/panel.rs"),
            include_str!("search/ui.rs"),
            include_str!("here.rs"),
            include_str!("pins.rs"),
            include_str!("printui.rs"),
            include_str!("routing/panel.rs"),
            include_str!("dialog.rs"),
        ];
        for e in TABLE {
            let name = format!("Command::{:?}", e.command);
            assert!(
                files.iter().any(|f| f.contains(&name)) || e.command == Command::Pan,
                "{name} is never handled"
            );
        }
    }
}
