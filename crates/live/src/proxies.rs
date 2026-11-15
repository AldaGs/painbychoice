//! Making proxies in the background and swapping them into the preview
//! (ADR 0029). The files and their naming are `motion_render::proxy`.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::*;

/// Proxy jobs: one thread per batch, one ffmpeg at a time within it.
pub(crate) struct Proxies {
    /// Preview from proxies when they exist.
    pub(crate) on: bool,
    tx: Sender<(PathBuf, Result<PathBuf, String>)>,
    rx: Receiver<(PathBuf, Result<PathBuf, String>)>,
    queued: std::collections::HashSet<PathBuf>,
    pub(crate) status: Option<String>,
}

impl Default for Proxies {
    fn default() -> Self {
        let (tx, rx) = channel();
        Proxies { on: true, tx, rx, queued: Default::default(), status: None }
    }
}

impl Proxies {
    /// Still making some.
    pub(crate) fn pending(&self) -> usize {
        self.queued.len()
    }
}

impl App {
    /// Start proxies for every video in the project that is big enough to
    /// want one and doesn't have one yet.
    pub(crate) fn queue_proxies(&mut self) {
        let todo: Vec<PathBuf> = self
            .project
            .assets
            .values()
            .filter(|a| a.kind == motion_core::AssetKind::Video && motion_render::proxy::wanted(a.width, a.height))
            .map(|a| a.path.clone())
            .filter(|p| p.exists() && motion_render::proxy::existing(p).is_none() && !self.proxies.queued.contains(p))
            .collect();
        if todo.is_empty() {
            return;
        }
        self.proxies.queued.extend(todo.iter().cloned());
        let tx = self.proxies.tx.clone();
        let _ = std::thread::Builder::new().name("proxies".into()).spawn(move || {
            for p in todo {
                let r = motion_render::proxy::make(&p);
                if tx.send((p, r)).is_err() {
                    return;
                }
            }
        });
    }

    /// Take finished proxies. Whether any arrived (the preview should redraw).
    pub(crate) fn poll_proxies(&mut self) -> bool {
        let mut any = false;
        while let Ok((src, r)) = self.proxies.rx.try_recv() {
            self.proxies.queued.remove(&src);
            if let Err(e) = r {
                self.proxies.status = Some(e);
            }
            any = true;
        }
        if any {
            // The preview may now read a smaller file for these clips.
            self.footage.forget_proxies();
            if self.proxies.pending() == 0 && self.proxies.status.as_deref().is_none_or(|s| !s.contains("failed")) {
                self.proxies.status = None;
            }
        }
        any
    }

    pub(crate) fn set_proxies(&mut self, on: bool) {
        self.proxies.on = on;
        self.footage.proxies = on;
        self.footage.forget_proxies();
    }
}
