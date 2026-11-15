//! The live link: one `.pbc`, two writers — the open editor and an agent
//! editing the file from outside (ADR 0024).
//!
//! Three rules, all here as free functions so they are testable without a
//! window:
//!
//! - **Reload is an undo step.** A foreign write swaps in through
//!   [`take`], which records the editor's document as it stood, so Undo takes
//!   the agent's change back out. Snapshot undo (ADR 0014) makes this free.
//! - **Never clobber.** [`Disk::conflict`] says whether the file changed since
//!   the editor last read or wrote it; a save that would overwrite a foreign
//!   write asks first.
//! - **Nothing invalid gets in.** [`parse`] migrates (invariant 8) and refuses a
//!   project whose ids collide, keeping the editor's document instead.
//!
//! Change detection is an mtime poll, not a `notify` watcher: one file, a
//! 250ms poll, and no dependency. A half-written file fails to parse and is
//! picked up again by the write that finishes it, which is the debounce.

use std::path::Path;
use std::time::SystemTime;

use motion_core::{CompId, Node};

use crate::history::History;
use crate::MProject;

/// What the editor last saw of its file: the text it read or wrote, and the
/// mtime it last looked at.
#[derive(Default)]
pub(crate) struct Disk {
    pub(crate) text: String,
    mtime: Option<SystemTime>,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

impl Disk {
    /// The editor just read or wrote `text` at `path`: that is the baseline.
    pub(crate) fn synced(path: &Path, text: String) -> Self {
        Disk { text, mtime: mtime(path) }
    }

    /// The file's text, if someone else changed it since the last call. Marks
    /// the new mtime seen either way, so a file that fails to parse is not
    /// re-read every poll — only when it is written again.
    pub(crate) fn poll(&mut self, path: &Path) -> Option<String> {
        let m = mtime(path)?;
        if self.mtime == Some(m) {
            return None;
        }
        self.mtime = Some(m);
        let text = std::fs::read_to_string(path).ok()?;
        (text != self.text).then_some(text)
    }

    /// Whether writing now would overwrite a change the editor has not taken.
    /// A missing file is not a conflict: there is nothing to lose.
    pub(crate) fn conflict(&self, path: &Path) -> bool {
        std::fs::read_to_string(path).is_ok_and(|t| t != self.text)
    }
}

/// The `rev` a file carries, 0 for one written before revs existed.
pub(crate) fn rev_of(text: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(text).ok().and_then(|v| v["rev"].as_u64()).unwrap_or(0)
}

/// A foreign write as a migrated, valid project.
pub(crate) fn parse(text: &str) -> Result<MProject, String> {
    let (mut project, _layout) = crate::app::parse_save(text)?;
    project.migrate();
    if let Some((comp, id)) = duplicate_id(&project) {
        return Err(format!("comp {} has two layers with id {id}", comp.0));
    }
    if project.comp(project.root).is_none() {
        return Err("root names a comp that does not exist".into());
    }
    Ok(project)
}

/// A layer id used twice within one comp — the one mistake an agent adding
/// layers by hand is likely to make, and one that would alias two layers.
pub(crate) fn duplicate_id(project: &MProject) -> Option<(CompId, u64)> {
    fn walk(n: &Node, seen: &mut std::collections::HashSet<u64>) -> Option<u64> {
        if !seen.insert(n.id.0) {
            return Some(n.id.0);
        }
        n.children.iter().find_map(|c| walk(c, seen))
    }
    project.comps.iter().find_map(|(id, c)| walk(&c.root, &mut Default::default()).map(|n| (*id, n)))
}

/// Swap `incoming` in as one undoable step. Returns whether anything changed.
pub(crate) fn take(history: &mut History, project: &mut MProject, incoming: MProject) -> bool {
    if *project == incoming {
        return false;
    }
    let before = std::mem::replace(project, incoming);
    history.record(before, "External edit", false);
    true
}
