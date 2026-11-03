//! The light or dark look the desktop asks for.
//!
//! On Windows (and macOS) the window system reports it and egui passes it on
//! (`Context::system_theme`), including when it changes. winit 0.30 reports nothing on Linux,
//! so there it is read from the desktop's settings portal (`org.freedesktop.appearance`
//! `color-scheme`, which GNOME, KDE and others set), or GNOME's own setting where there is no
//! portal. Both are read with the tools a desktop with them already has (`gdbus`,
//! `gsettings`), so the app links no D-Bus library, and re-read every few seconds, which is
//! cheap beside what the user's change of theme costs them.

use eframe::egui;

/// The system's choice, kept up to date.
pub struct Watch {
    #[cfg(target_os = "linux")]
    seen: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

impl Watch {
    /// Starts watching; `ctx` is woken when the choice changes.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    pub fn start(ctx: &egui::Context) -> Self {
        #[cfg(target_os = "linux")]
        {
            use std::sync::{Arc, atomic::AtomicU8};
            let seen = Arc::new(AtomicU8::new(linux::UNKNOWN));
            let (shared, ctx) = (seen.clone(), ctx.clone());
            // A failed spawn leaves the choice unknown, which is light.
            let _ = std::thread::Builder::new()
                .name("system-theme".into())
                .spawn(move || linux::watch(&shared, &ctx));
            Self { seen }
        }
        #[cfg(not(target_os = "linux"))]
        Self {}
    }

    /// What the system asks for, if it says.
    pub fn get(&self, ctx: &egui::Context) -> Option<egui::Theme> {
        #[cfg(target_os = "linux")]
        if let Some(t) = linux::decode(self.seen.load(std::sync::atomic::Ordering::Relaxed)) {
            return Some(t);
        }
        ctx.system_theme()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::time::Duration;

    use eframe::egui::{self, Theme};

    pub const UNKNOWN: u8 = 0;
    const LIGHT: u8 = 1;
    const DARK: u8 = 2;
    const POLL: Duration = Duration::from_secs(3);

    pub fn decode(v: u8) -> Option<Theme> {
        match v {
            LIGHT => Some(Theme::Light),
            DARK => Some(Theme::Dark),
            _ => None,
        }
    }

    pub fn watch(seen: &AtomicU8, ctx: &egui::Context) {
        let mut tools = 0;
        loop {
            let (now, used) = read(tools);
            tools = used;
            if seen.swap(now, Ordering::Relaxed) != now {
                ctx.request_repaint();
            }
            // No tool answers (none installed, or no session bus): look again now and then.
            std::thread::sleep(if used == NONE { POLL * 10 } else { POLL });
        }
    }

    const PORTAL: u8 = 1;
    const GSETTINGS: u8 = 2;
    const NONE: u8 = 3;

    /// The choice, and the tool that gave it (tried first next time).
    fn read(first: u8) -> (u8, u8) {
        let order = if first == GSETTINGS {
            [GSETTINGS, PORTAL]
        } else {
            [PORTAL, GSETTINGS]
        };
        for tool in order {
            let out = match tool {
                PORTAL => run(
                    "gdbus",
                    &[
                        "call",
                        "--session",
                        "--dest",
                        "org.freedesktop.portal.Desktop",
                        "--object-path",
                        "/org/freedesktop/portal/desktop",
                        "--method",
                        "org.freedesktop.portal.Settings.Read",
                        "org.freedesktop.appearance",
                        "color-scheme",
                    ],
                )
                .map(|t| parse_portal(&t)),
                _ => run(
                    "gsettings",
                    &["get", "org.gnome.desktop.interface", "color-scheme"],
                )
                .map(|t| parse_gsettings(&t)),
            };
            // A tool that ran but has no preference (0, 'default') does not settle it.
            if let Some(v @ (LIGHT | DARK)) = out {
                return (v, tool);
            }
        }
        (UNKNOWN, NONE)
    }

    fn run(program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `(<<uint32 1>>,)`: 1 is dark, 2 is light, 0 no preference.
    pub fn parse_portal(text: &str) -> u8 {
        let digit = text
            .split("uint32")
            .nth(1)
            .and_then(|t| t.trim_start().chars().next());
        match digit {
            Some('1') => DARK,
            Some('2') => LIGHT,
            _ => UNKNOWN,
        }
    }

    /// `'prefer-dark'`, `'prefer-light'` or `'default'`.
    pub fn parse_gsettings(text: &str) -> u8 {
        if text.contains("prefer-dark") {
            DARK
        } else if text.contains("prefer-light") {
            LIGHT
        } else {
            UNKNOWN
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn reads_what_the_tools_print() {
            assert_eq!(parse_portal("(<<uint32 1>>,)\n"), DARK);
            assert_eq!(parse_portal("(<<uint32 2>>,)\n"), LIGHT);
            assert_eq!(parse_portal("(<<uint32 0>>,)\n"), UNKNOWN);
            assert_eq!(parse_portal("garbage"), UNKNOWN);
            assert_eq!(parse_gsettings("'prefer-dark'\n"), DARK);
            assert_eq!(parse_gsettings("'prefer-light'\n"), LIGHT);
            assert_eq!(parse_gsettings("'default'\n"), UNKNOWN);
        }
    }
}
