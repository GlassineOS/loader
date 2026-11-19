// SPDX-License-Identifier: GPL-2.0-or-later
//! What the start menu draws (loader spec 004 FR-008), as the boards in GlassineOS/ui
//! `designs/startmenu` give it: a card per entry with an icon, a name and one plain
//! line, the reason the menu opened, the foot. The words live here so they are tested
//! with the lists; the log keeps the entries' labels ([`crate::deploys::Entry::label`]).
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::deploys::{self, Action, Entry};
use crate::menu::Why;

/// The cards' icons, in the order of the shim's `SHIM_ICON_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Icon {
    Mark = 0,
    Previous = 1,
    More = 2,
    Kernel = 3,
    Other = 4,
    Recovery = 5,
    Deployment = 6,
    Back = 7,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub name: String,
    pub line: String,
    pub icon: Icon,
}

/// The most cards a list shows (the panel's height; FR-008).
pub const CARDS_MAX: usize = 6;

pub const TITLE: &str = "Start GlassineOS";
pub const TITLE_MORE: &str = "More deployments";
pub const REASON_MORE: &str = "Choose a version to start once.";
pub const FOOT: &str = "If you don\u{2019}t choose, GlassineOS starts by itself in a minute.";
pub const FOOT_CUT: &str = "Older versions that don\u{2019}t fit here stay kept. If you don\u{2019}t choose, GlassineOS starts by itself in a minute.";
pub const FOOT_NO_TOUCH: &str = "Touch isn\u{2019}t working this time. GlassineOS starts by itself in a minute.";
/// The default card's line once there are no taps.
pub const LINE_NO_TOUCH: &str = "Starts by itself in a minute";

/// The line under the title: why the menu opened, in plain words.
pub fn reason(why: Why) -> &'static str {
    match why {
        Why::Request => "Choose how to start.",
        Why::Unmarked => "The last start didn\u{2019}t finish, so this menu opened. Choose how to start this time.",
    }
}

/// The card of a list entry.
pub fn card(e: &Entry) -> Card {
    let c = |name: &str, line: String, icon| Card { name: name.into(), line, icon };
    match (&e.action, &e.deploy) {
        (Action::Continue, _) => c("GlassineOS", "The usual start, chosen for you".into(), Icon::Mark),
        (Action::Deploy(_), Some(d)) if d.state == "previous" => {
            c("Previous deployment", format!("{} \u{b7} as before the last update", deploys::shown_name(d)), Icon::Previous)
        }
        (Action::Deploy(_), Some(d)) => {
            let line = match d.state.as_str() {
                "candidate" => "New, not started yet",
                "failed" => "Didn\u{2019}t finish starting last time",
                _ => "Kept on this device",
            };
            c(&deploys::shown_name(d), line.into(), Icon::Deployment)
        }
        (Action::Deploy(id), None) => c(id, String::new(), Icon::Deployment),
        (Action::More, _) => c("More deployments", "Other versions kept on this device".into(), Icon::More),
        (Action::PreviousKernel, _) => c("Previous kernel", "The kernel from before the last update".into(), Icon::Kernel),
        (Action::Other(n), _) => {
            let l = deploys::other_label(n);
            c(&l, format!("Start {l} this once"), Icon::Other)
        }
        (Action::Recovery, _) => c("Recovery", "Repair, reset or back up GlassineOS".into(), Icon::Recovery),
        (Action::Back, _) => c("Back", "To the start menu".into(), Icon::Back),
    }
}

/// Which of `n` entries a list shows: all when they fit, else the first
/// `CARDS_MAX - 1` and the last (Back stays).
pub fn fit(n: usize) -> Vec<usize> {
    if n <= CARDS_MAX {
        (0..n).collect()
    } else {
        (0..CARDS_MAX - 1).chain(core::iter::once(n - 1)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record;
    use crate::sha256_hex;

    fn rec(body: &str) -> record::Record {
        let t = format!("format=1\nseq=1\n{body}");
        record::parse(format!("{t}check={}\n", sha256_hex(t.as_bytes())).as_bytes()).unwrap()
    }

    #[test]
    fn every_entry_has_its_card() {
        let r = rec("previous=0123456789abcdef\ndeploys=4\ndeploy1=a previous e4ffb6ec 2026-11-15\ndeploy2=b candidate f00dcafe 2026-11-18\ndeploy3=c failed f00dcafe 2026-11-16\ndeploy4=k kept\n");
        let m: Vec<Card> = deploys::main_list(Some(&r), Some("android")).iter().map(card).collect();
        let names: Vec<&str> = m.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["GlassineOS", "Previous deployment", "More deployments", "Previous kernel", "Android", "Recovery"]);
        assert_eq!(m[1].line, "e4ffb6ec 2026-11-15 \u{b7} as before the last update");
        assert_eq!(m[4].line, "Start Android this once");
        let icons: Vec<Icon> = m.iter().map(|c| c.icon).collect();
        assert_eq!(icons, [Icon::Mark, Icon::Previous, Icon::More, Icon::Kernel, Icon::Other, Icon::Recovery]);
        let more: Vec<Card> = deploys::more_list(&r).iter().map(card).collect();
        assert_eq!(more[0], Card { name: "f00dcafe 2026-11-18".into(), line: "New, not started yet".into(), icon: Icon::Deployment });
        assert_eq!(more[1].line, "Didn\u{2019}t finish starting last time");
        assert_eq!((more[2].name.as_str(), more[2].line.as_str()), ("k", "Kept on this device"), "no label: the id");
        assert_eq!(more[3], Card { name: "Back".into(), line: "To the start menu".into(), icon: Icon::Back });
    }

    #[test]
    fn a_long_list_keeps_back() {
        assert_eq!(fit(3), [0, 1, 2]);
        assert_eq!(fit(6), [0, 1, 2, 3, 4, 5]);
        assert_eq!(fit(9), [0, 1, 2, 3, 4, 8]);
        assert_eq!(reason(Why::Request), "Choose how to start.");
        assert!(reason(Why::Unmarked).starts_with("The last start didn\u{2019}t finish"));
    }
}
