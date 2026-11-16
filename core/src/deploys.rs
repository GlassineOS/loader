// SPDX-License-Identifier: GPL-2.0-or-later
//! The deployment list of the loader record (`deploys`, `deploy1` … `deploy<n>`;
//! `boot-partition.md`, "Keys") and the start-options list it feeds (010 FR-039,
//! FR-040).
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::record::Record;

pub const MAX: usize = 32;
const LABEL_MAX: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    pub id: String,
    /// `previous`, `candidate`, `failed` or `kept`.
    pub state: String,
    pub label: String,
}

/// Deployment ids: `[A-Za-z0-9._-]+`, never starting with a dot (deployments.md).
pub fn is_deployment_id(id: &str) -> bool {
    !id.is_empty() && !id.starts_with('.') && id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn entry(v: &str) -> Option<Deployment> {
    let (id, rest) = v.split_once(' ')?;
    let (state, label) = rest.split_once(' ').unwrap_or((rest, ""));
    let ok_state = matches!(state, "previous" | "candidate" | "failed" | "kept");
    let ok_label = label.len() <= LABEL_MAX && label.bytes().all(|c| (0x20..=0x7e).contains(&c));
    (is_deployment_id(id) && ok_state && ok_label).then(|| Deployment { id: id.into(), state: state.into(), label: label.into() })
}

/// The list, in its order; entries that break the form are left out.
pub fn list(rec: &Record) -> Vec<Deployment> {
    let n: usize = rec.get("deploys").parse().unwrap_or(0);
    (1..=n.min(MAX)).filter_map(|i| entry(rec.get(&format!("deploy{i}")))).collect()
}

/// What a list entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Start what would have started without the key.
    Continue,
    /// The same, with `glassine.deploy.once=<id>`.
    Deploy(String),
    /// Open the other deployments.
    More,
    PreviousKernel,
    Recovery,
    /// Back to the first list.
    Back,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: String,
    pub action: Action,
}

fn e(label: String, action: Action) -> Entry {
    Entry { label, action }
}

fn name(d: &Deployment) -> String {
    if d.label.is_empty() { d.id.clone() } else { d.label.clone() }
}

/// The first list (010 FR-039): GlassineOS, the previous deployment, "More", the
/// previous kernel, the recovery. An entry with nothing behind it is left out. `rec` is
/// the record in use, if any. The firmware's set-up of FR-039 is not offered: it needs
/// a firmware variable written at a start, which 010 FR-031 rules out.
pub fn main_list(rec: Option<&Record>) -> Vec<Entry> {
    let mut out = Vec::new();
    if let Some(r) = rec {
        out.push(e("GlassineOS".into(), Action::Continue));
        let ds = list(r);
        if let Some(p) = ds.iter().find(|d| d.state == "previous") {
            out.push(e(format!("Previous deployment: {}", name(p)), Action::Deploy(p.id.clone())));
        }
        if ds.iter().any(|d| d.state != "previous") {
            out.push(e("More deployments".into(), Action::More));
        }
        if !r.get("previous").is_empty() {
            out.push(e("Previous kernel".into(), Action::PreviousKernel));
        }
    }
    out.push(e("Recovery".into(), Action::Recovery));
    out
}

/// The other deployments (010 FR-040): the candidate, the last failed one and the kept
/// ones, in the record's order, then "Back".
pub fn more_list(rec: &Record) -> Vec<Entry> {
    let mut out: Vec<Entry> = list(rec)
        .iter()
        .filter(|d| d.state != "previous")
        .map(|d| {
            let s = match d.state.as_str() {
                "candidate" => "Candidate",
                "failed" => "Failed",
                _ => "Kept",
            };
            e(format!("{s}: {}", name(d)), Action::Deploy(d.id.to_string()))
        })
        .collect();
    out.push(e("Back".into(), Action::Back));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record;
    use crate::sha256_hex;

    fn rec(body: &str) -> Record {
        let t = format!("format=1\nseq=1\n{body}");
        record::parse(format!("{t}check={}\n", sha256_hex(t.as_bytes())).as_bytes()).unwrap()
    }

    #[test]
    fn the_list_comes_from_the_record() {
        let r = rec("previous=0123456789abcdef\ndeploys=4\ndeploy1=d1 previous 3.24.1 2026-11-01\ndeploy2=d2 candidate 3.24.2 2026-11-10\ndeploy3=.bad kept x\ndeploy4=d4 kept\n");
        let l = list(&r);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0].label, "3.24.1 2026-11-01");
        assert_eq!(l[2], Deployment { id: "d4".into(), state: "kept".into(), label: "".into() });
        let m = main_list(Some(&r));
        let acts: Vec<&Action> = m.iter().map(|x| &x.action).collect();
        assert_eq!(acts, [&Action::Continue, &Action::Deploy("d1".into()), &Action::More, &Action::PreviousKernel, &Action::Recovery]);
        assert_eq!(m[1].label, "Previous deployment: 3.24.1 2026-11-01");
        let more = more_list(&r);
        assert_eq!(more.len(), 3);
        assert_eq!(more[0].label, "Candidate: 3.24.2 2026-11-10");
        assert_eq!(more[1].label, "Kept: d4");
        assert_eq!(more[2].action, Action::Back);
    }

    #[test]
    fn bad_entries_and_counts_are_left_out() {
        let label65 = "x".repeat(65);
        let r = rec(&format!("deploys=40\ndeploy1=d1 staged x\ndeploy2=d2 kept {label65}\ndeploy3=d3 kept caf\u{e9}\ndeploy4=d 4 kept\ndeploy33=d33 kept x\n"));
        assert!(list(&r).is_empty(), "{:?}", list(&r));
        assert!(list(&rec("deploys=x\ndeploy1=d1 kept x\n")).is_empty());
        let m = main_list(Some(&rec("previous=\ndeploys=0\n")));
        assert_eq!(m.iter().map(|x| x.label.as_str()).collect::<Vec<_>>(), ["GlassineOS", "Recovery"]);
        let m = main_list(None);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].action, Action::Recovery);
    }
}
