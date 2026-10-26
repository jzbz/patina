//! Keyboard bindings in mpv's `input.conf` format.
//!
//! Bindings map a key combination to mpv commands, or to a Patina action
//! written as `#@patina KEY action` (IINA's `#@iina KEY action` lines are
//! read the same way, so IINA key binding files can be used directly).

use std::collections::HashMap;

pub const SHIFT: u8 = 1;
pub const CTRL: u8 = 2;
pub const ALT: u8 = 4;
pub const META: u8 = 8;

/// A normalized key combination, e.g. `Ctrl+LEFT` or `A` (Shift+a).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub mods: u8,
    pub key: String,
}

impl KeyCombo {
    fn normalized(mut mods: u8, key: &str) -> Self {
        let mut chars = key.chars();
        let key = match (chars.next(), chars.next()) {
            // A single character already carries Shift ("A", "("), so the
            // modifier is dropped, except that Shift+letter means uppercase.
            (Some(c), None) => {
                if mods & SHIFT != 0 {
                    mods &= !SHIFT;
                    c.to_uppercase().collect()
                } else {
                    c.to_string()
                }
            }
            _ => match key.to_ascii_uppercase().as_str() {
                "SHARP" => "#".to_owned(),
                "PLUS" => "+".to_owned(),
                "MINUS" => "-".to_owned(),
                "RETURN" => "ENTER".to_owned(),
                "ESCAPE" => "ESC".to_owned(),
                "BACKSPACE" => "BS".to_owned(),
                "PAGEUP" => "PGUP".to_owned(),
                "PAGEDOWN" => "PGDWN".to_owned(),
                upper => upper.to_owned(),
            },
        };
        Self { mods, key }
    }

    /// Parses an input.conf key name such as `Ctrl+Shift+LEFT` or `Meta++`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut mods = 0;
        let mut rest = text;
        while let Some((head, tail)) = rest.split_once('+') {
            if tail.is_empty() {
                // "Ctrl++": the key itself is '+'.
                break;
            }
            let modifier = match head.to_ascii_lowercase().as_str() {
                "shift" => SHIFT,
                "ctrl" | "control" => CTRL,
                "alt" | "option" => ALT,
                "meta" | "cmd" | "command" | "super" => META,
                _ => break,
            };
            mods |= modifier;
            rest = tail;
        }
        (!rest.is_empty()).then(|| Self::normalized(mods, rest))
    }

    /// Builds a combination from a Slint key event.
    pub fn from_event(text: &str, shift: bool, ctrl: bool, alt: bool, meta: bool) -> Option<Self> {
        let mut mods = 0;
        if shift {
            mods |= SHIFT;
        }
        if ctrl {
            mods |= CTRL;
        }
        if alt {
            mods |= ALT;
        }
        if meta {
            mods |= META;
        }
        let mut chars = text.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        let named = match c {
            ' ' => "SPACE",
            '\n' | '\r' => "ENTER",
            '\t' => "TAB",
            '\u{1b}' => "ESC",
            '\u{8}' => "BS",
            '\u{7f}' => "DEL",
            '\u{F700}' => "UP",
            '\u{F701}' => "DOWN",
            '\u{F702}' => "LEFT",
            '\u{F703}' => "RIGHT",
            '\u{F727}' => "INS",
            '\u{F729}' => "HOME",
            '\u{F72B}' => "END",
            '\u{F72C}' => "PGUP",
            '\u{F72D}' => "PGDWN",
            '\u{F730}' => "PAUSE",
            '\u{F735}' => "MENU",
            '\u{F704}'..='\u{F71B}' => {
                return Some(Self {
                    mods,
                    key: format!("F{}", c as u32 - 0xF704 + 1),
                });
            }
            // Lone modifier presses.
            '\u{10}'..='\u{18}' => return None,
            // Ctrl+letter can arrive as an ASCII control character.
            '\u{1}'..='\u{1a}' if ctrl => {
                return Some(Self::normalized(
                    mods,
                    &((c as u8 + b'a' - 1) as char).to_string(),
                ));
            }
            c if c.is_control() || ('\u{F700}'..='\u{F8FF}').contains(&c) => return None,
            _ => return Some(Self::normalized(mods, &c.to_string())),
        };
        Some(Self {
            mods,
            key: named.to_owned(),
        })
    }

    /// A readable label, e.g. "Ctrl+Shift+→".
    pub fn label(&self) -> String {
        let mut label = String::new();
        let primary_is_meta = cfg!(target_os = "macos");
        for (bit, name) in [
            (CTRL, "Ctrl"),
            (ALT, if primary_is_meta { "Option" } else { "Alt" }),
            (SHIFT, "Shift"),
            (META, if primary_is_meta { "Cmd" } else { "Super" }),
        ] {
            if self.mods & bit != 0 {
                label.push_str(name);
                label.push('+');
            }
        }
        label.push_str(match self.key.as_str() {
            "SPACE" => "Space",
            "ENTER" => "Enter",
            "ESC" => "Esc",
            "BS" => "Backspace",
            "DEL" => "Delete",
            "TAB" => "Tab",
            "LEFT" => "←",
            "RIGHT" => "→",
            "UP" => "↑",
            "DOWN" => "↓",
            "PGUP" => "Page Up",
            "PGDWN" => "Page Down",
            "HOME" => "Home",
            "END" => "End",
            "INS" => "Insert",
            key => key,
        });
        label
    }
}

/// What a key runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Binding {
    /// One or more mpv commands (separated by `;` in input.conf).
    Mpv(Vec<Vec<String>>),
    /// A Patina action, e.g. "open-file" or "window-scale 2".
    Patina(String),
}

impl Binding {
    pub fn describe(&self) -> String {
        match self {
            Binding::Mpv(commands) => commands
                .iter()
                .map(|command| command.join(" "))
                .collect::<Vec<_>>()
                .join("; "),
            Binding::Patina(action) => action.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Bindings {
    map: HashMap<KeyCombo, Binding>,
    /// Bindings in file order, for the shortcuts list.
    order: Vec<KeyCombo>,
}

impl Bindings {
    pub fn parse(conf: &str) -> Self {
        let mut bindings = Self::default();
        for line in conf.lines() {
            let line = line.trim();
            let (patina, line) = match line
                .strip_prefix("#@patina")
                .or_else(|| line.strip_prefix("#@iina"))
            {
                Some(rest) => (true, rest.trim_start()),
                None if line.starts_with('#') || line.is_empty() => continue,
                None => (false, line),
            };
            let Some((key, command)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let Some(combo) = KeyCombo::parse(key) else {
                log::warn!("unknown key in bindings: {key}");
                continue;
            };
            let command = strip_comment(command).trim();
            if command.is_empty() || command == "ignore" {
                bindings.map.remove(&combo);
                bindings.order.retain(|existing| existing != &combo);
                continue;
            }
            let binding = if patina {
                Binding::Patina(command.to_owned())
            } else {
                match split_commands(command) {
                    Some(commands) if !commands.is_empty() => Binding::Mpv(commands),
                    _ => {
                        log::warn!("could not parse binding: {line}");
                        continue;
                    }
                }
            };
            if bindings.map.insert(combo.clone(), binding).is_none() {
                bindings.order.push(combo);
            }
        }
        bindings
    }

    pub fn get(&self, combo: &KeyCombo) -> Option<&Binding> {
        self.map.get(combo)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&KeyCombo, &Binding)> {
        self.order
            .iter()
            .filter_map(|combo| self.map.get(combo).map(|binding| (combo, binding)))
    }
}

/// Removes a trailing `# comment` that is outside quotes.
fn strip_comment(text: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, c) in text.char_indices() {
        match (quote, c) {
            (Some('"'), '\\') if !escaped => {
                escaped = true;
                continue;
            }
            (Some(q), c) if c == q && !escaped => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') => return &text[..index],
            _ => {}
        }
        escaped = false;
    }
    text
}

/// Splits an mpv command line into commands and arguments, following
/// input.conf quoting: "double" with backslash escapes, 'single' literal.
pub fn split_commands(text: &str) -> Option<Vec<Vec<String>>> {
    let mut commands = Vec::new();
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_arg = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            'n' => current.push('\n'),
                            't' => current.push('\t'),
                            other => current.push(other),
                        },
                        other => current.push(other),
                    }
                }
            }
            '\'' => {
                in_arg = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        other => current.push(other),
                    }
                }
            }
            ';' => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
                if !args.is_empty() {
                    commands.push(std::mem::take(&mut args));
                }
            }
            c if c.is_whitespace() => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            c => {
                in_arg = true;
                current.push(c);
            }
        }
    }
    if in_arg {
        args.push(current);
    }
    if !args.is_empty() {
        commands.push(args);
    }
    Some(commands)
}

/// Patina's default bindings, following IINA's defaults with the platform's
/// primary modifier: Command on macOS, Ctrl elsewhere.
pub fn default_conf() -> String {
    let (primary, secondary) = if cfg!(target_os = "macos") {
        ("Meta", "Ctrl+Meta")
    } else {
        ("Ctrl", "Ctrl+Alt")
    };
    DEFAULT_CONF
        .replace("SECONDARY+", &format!("{secondary}+"))
        .replace("PRIMARY+", &format!("{primary}+"))
}

const DEFAULT_CONF: &str = r##"# Patina key bindings, in mpv input.conf format.
# Lines starting with "#@patina KEY action" run Patina actions.

#@patina PRIMARY+o open-file
#@patina PRIMARY+Shift+o open-url
#@patina PRIMARY+w close-window
#@patina PRIMARY+q quit
#@patina PRIMARY+n new-window
#@patina PRIMARY+j jump-to
#@patina PRIMARY+Shift+v video-panel
#@patina PRIMARY+Shift+a audio-panel
#@patina PRIMARY+Shift+s sub-panel
#@patina PRIMARY+Shift+p playlist-panel
#@patina PRIMARY+Shift+c chapter-panel
#@patina PRIMARY+Shift+r reveal-file
#@patina PRIMARY+3 fit-to-screen
#@patina PRIMARY+- smaller-window
#@patina PRIMARY+= bigger-window
#@patina SECONDARY+p toggle-pip
#@patina F1 show-shortcuts
#@patina F11 toggle-fullscreen

SECONDARY+v cycle video
SECONDARY+s cycle sub
SECONDARY+a cycle audio

SPACE cycle pause
PRIMARY+. stop

RIGHT seek 5
LEFT seek -5
Alt+RIGHT frame-step
Alt+LEFT frame-back-step
Shift+LEFT sub-seek -1
Shift+RIGHT sub-seek 1

PRIMARY+s screenshot
PRIMARY+l ab-loop
PRIMARY+Shift+l cycle-values loop-file "inf" "no"

PRIMARY+RIGHT playlist-next
PRIMARY+LEFT playlist-prev
PRIMARY+> add chapter 1
PRIMARY+< add chapter -1

PRIMARY+[ multiply speed 0.5
PRIMARY+] multiply speed 2.0
Alt+PRIMARY+[ multiply speed 0.9091
Alt+PRIMARY+] multiply speed 1.1
PRIMARY+\ set speed 1.0

PRIMARY+0 set window-scale 0.5
PRIMARY+1 set window-scale 1
PRIMARY+2 set window-scale 2
SECONDARY+f cycle fullscreen
SECONDARY+t cycle ontop

UP add volume 5
DOWN add volume -5
Alt+UP add volume 1
Alt+DOWN add volume -1
PRIMARY+/ cycle mute

( add audio-delay 0.5
) add audio-delay -0.5
Alt+( add audio-delay 0.1
Alt+) add audio-delay -0.1
_ set audio-delay 0

Z add sub-delay -0.5
X add sub-delay 0.5
Alt+Z add sub-delay -0.1
Alt+X add sub-delay 0.1
C set sub-delay 0

ESC set fullscreen no
ENTER set fullscreen yes

# mpv's traditional keys.
q quit
p cycle pause
. frame-step
, frame-back-step
o show-progress
P show-progress
m cycle mute
Shift+PGUP seek 600
Shift+PGDWN seek -600
G add sub-scale 0.1
F add sub-scale -0.1
r add sub-pos -1
R add sub-pos 1
t add sub-pos 1
v cycle sub-visibility
Alt+v cycle secondary-sub-visibility
f cycle fullscreen
E cycle edition
[ multiply speed 0.9091
] multiply speed 1.1
BS set speed 1.0
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_normalize_like_mpv() {
        assert_eq!(KeyCombo::parse("Shift+a"), KeyCombo::parse("A"));
        assert_eq!(KeyCombo::parse("Shift+("), KeyCombo::parse("("));
        assert_eq!(
            KeyCombo::parse("Ctrl+Shift+LEFT"),
            Some(KeyCombo {
                mods: CTRL | SHIFT,
                key: "LEFT".into()
            })
        );
        assert_eq!(
            KeyCombo::parse("Meta++"),
            Some(KeyCombo {
                mods: META,
                key: "+".into()
            })
        );
        assert_eq!(KeyCombo::parse("SHARP"), KeyCombo::parse("#"));
        assert_eq!(KeyCombo::parse("Alt+Shift+)"), KeyCombo::parse("Alt+)"));
        assert_eq!(KeyCombo::parse("enter"), KeyCombo::parse("ENTER"));
    }

    #[test]
    fn slint_events_map_to_the_same_combinations() {
        let event = |text, shift, ctrl| KeyCombo::from_event(text, shift, ctrl, false, false);
        assert_eq!(event(" ", false, false), KeyCombo::parse("SPACE"));
        assert_eq!(
            event("\u{F703}", false, true),
            KeyCombo::parse("Ctrl+RIGHT")
        );
        assert_eq!(event("A", true, false), KeyCombo::parse("Shift+a"));
        assert_eq!(event("(", true, false), KeyCombo::parse("Shift+("));
        assert_eq!(event("\u{f}", false, true), KeyCombo::parse("Ctrl+o"));
        assert_eq!(event("o", false, true), KeyCombo::parse("Ctrl+o"));
        assert_eq!(event("\u{F70E}", false, false), KeyCombo::parse("F11"));
        assert_eq!(event("\u{10}", true, false), None);
    }

    #[test]
    fn commands_split_on_quotes_and_semicolons() {
        assert_eq!(
            split_commands(r#"cycle-values loop-file "inf" "no""#),
            Some(vec![vec![
                "cycle-values".into(),
                "loop-file".into(),
                "inf".into(),
                "no".into()
            ]])
        );
        assert_eq!(
            split_commands(r#"show-text "a \"b\"" ; seek 5"#),
            Some(vec![
                vec!["show-text".into(), "a \"b\"".into()],
                vec!["seek".into(), "5".into()]
            ])
        );
        assert_eq!(
            split_commands("seek 'it''s'"),
            Some(vec![vec!["seek".into(), "its".into()]])
        );
        assert_eq!(split_commands("seek \"unterminated"), None);
    }

    #[test]
    fn conf_files_parse_comments_and_patina_actions() {
        let bindings = Bindings::parse(
            "# comment\nRIGHT seek 5   # forward\n#@iina Meta+o open-file\nq ignore\nq quit\nx ignore\n",
        );
        assert_eq!(
            bindings.get(&KeyCombo::parse("RIGHT").unwrap()),
            Some(&Binding::Mpv(vec![vec!["seek".into(), "5".into()]]))
        );
        assert_eq!(
            bindings.get(&KeyCombo::parse("Meta+o").unwrap()),
            Some(&Binding::Patina("open-file".into()))
        );
        assert_eq!(
            bindings.get(&KeyCombo::parse("q").unwrap()),
            Some(&Binding::Mpv(vec![vec!["quit".into()]]))
        );
        assert!(bindings.get(&KeyCombo::parse("x").unwrap()).is_none());
        assert_eq!(bindings.iter().count(), 3);
    }

    #[test]
    fn default_bindings_parse_completely() {
        let conf = default_conf();
        let bindings = Bindings::parse(&conf);
        let lines = conf
            .lines()
            .filter(|line| {
                !line.is_empty() && (!line.starts_with('#') || line.starts_with("#@patina"))
            })
            .count();
        assert_eq!(
            bindings.iter().count(),
            lines,
            "every default binding is unique and valid"
        );
        assert!(!conf.contains("PRIMARY") && !conf.contains("SECONDARY"));
    }
}
