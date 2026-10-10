//! The GTK thread's half of the clipboard: watches the local `gdk::Clipboard`
//! for changes to offer to the server, reads a local item when the server
//! requests one, and mirrors the server's offer onto the local clipboard.
//!
//! The mirror is eager: the offered mimes are fetched when the offer
//! arrives and set as plain in-memory providers. A lazy provider whose
//! data is fetched on paste cannot work on macOS, where the pasteboard
//! fulfilment runs a nested main loop that never polls the
//! default-context task gdk4-rs spawns provider futures on — the fetch
//! would deadlock the whole UI. The eager fetch runs on the ordinary
//! main loop, where it always progresses.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use bytes::Bytes;
use gtk::gdk;
use gtk::gio::prelude::*;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use tokio::sync::mpsc::{Sender, UnboundedSender};
use tokio::sync::oneshot;

use gliff_proto::clipboard::{
    forwardable_mimes, is_file_mime, local_mimes_for_offer, offers_files, parse_uri_list,
    resolve_mime, CHUNK,
};
use gliff_proto::ClipboardFile;
use gliff_transport::clipboard::files::{list_files, LocalFiles};
use gliff_transport::clipboard::progress::{describe, Progress, State};

use crate::clipboard::ToWorker;

/// Longest `text/uri-list` we read to build an offer.
const URI_LIST_MAX: usize = 1024 * 1024;

/// Most we eagerly fetch of one offered item, so a giant selection
/// does not transfer when nobody may ever paste it.
const EAGER_MAX: usize = 8 * 1024 * 1024;

fn clipboard() -> Option<gdk::Clipboard> {
    gdk::Display::default().map(|d| d.clipboard())
}

thread_local! {
    /// The in-memory provider our last remote mirror set, so its own
    /// clipboard change is not echoed back to the server and a dead
    /// session's mirror can be dropped again.
    static MIRROR: RefCell<Option<gdk::ContentProvider>> = const { RefCell::new(None) };
}

fn holds_remote_offer(cb: &gdk::Clipboard) -> bool {
    MIRROR.with(|m| {
        m.borrow()
            .as_ref()
            .is_some_and(|ours| cb.content().is_some_and(|c| c == *ours))
    })
}

/// Drop the server's clipboard mirror, as when its session ends: the
/// offered content is unreachable, and on macOS a paste into a dead
/// mirror would otherwise stall the pasteboard fulfilment.
pub fn drop_remote_offer() {
    if let Some(cb) = clipboard() {
        if holds_remote_offer(&cb) {
            let _ = cb.set_content(gdk::ContentProvider::NONE);
        }
    }
}

/// Report every change of the local clipboard to the workers, except changes
/// made by our own proxy provider. Every running session gets the offer, so
/// any of them can paste it after a switch; nothing is sent until one does.
pub fn watch_local(senders: impl Fn() -> Vec<UnboundedSender<ToWorker>> + 'static) {
    let Some(cb) = clipboard() else {
        return;
    };
    let senders = Rc::new(senders);
    // Bumped per change so a slow file listing for a stale one is dropped.
    let selection_gen = Rc::new(Cell::new(0u64));
    cb.connect_changed(move |cb| {
        // Only our own proxy for the server's offer must not echo back;
        // a copy from another gliff widget is a real local change.
        let gen = selection_gen.get() + 1;
        selection_gen.set(gen);
        if holds_remote_offer(cb) {
            return;
        }
        let txs = senders();
        if txs.is_empty() {
            return;
        }
        let selection_gen = selection_gen.clone();
        let mimes: Vec<String> = cb
            .formats()
            .mime_types()
            .iter()
            .map(|m| m.to_string())
            .collect();
        let cb = cb.clone();
        glib::MainContext::default().spawn_local(async move {
            let files = if offers_files(&mimes) {
                local_files(&cb, &mimes).await
            } else {
                LocalFiles::default()
            };
            if selection_gen.get() != gen {
                return;
            }
            for tx in txs {
                let _ = tx.send(ToWorker::LocalOffer {
                    mime_types: forwardable_mimes(&mimes),
                    files: files.clone(),
                });
            }
        });
    });
}

/// List the files behind the local clipboard's file list, if readable. The
/// list is read as whichever file-list type the clipboard advertises; the
/// parser handles both bodies.
async fn local_files(cb: &gdk::Clipboard, mimes: &[String]) -> LocalFiles {
    let Some(list_mime) = mimes.iter().find(|m| is_file_mime(m)) else {
        return LocalFiles::default();
    };
    let Ok((stream, _)) = cb.read_future(&[list_mime], glib::Priority::DEFAULT).await else {
        return LocalFiles::default();
    };
    let mut body = Vec::new();
    while body.len() <= URI_LIST_MAX {
        match stream
            .read_bytes_future(64 * 1024, glib::Priority::DEFAULT)
            .await
        {
            Ok(b) if b.is_empty() => break,
            Ok(b) => body.extend_from_slice(&b),
            Err(_) => return LocalFiles::default(),
        }
    }
    if body.len() > URI_LIST_MAX {
        return LocalFiles::default();
    }
    let paths = parse_uri_list(&String::from_utf8_lossy(&body));
    if paths.is_empty() {
        return LocalFiles::default();
    }
    // The walk reads metadata for up to 10,000 entries, so it runs on its
    // own thread; blocking here would freeze rendering and input.
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(list_files(&paths));
    });
    match rx.await {
        Ok(Ok(files)) => files,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "clipboard files not offered");
            LocalFiles::default()
        }
        Err(_) => LocalFiles::default(),
    }
}

/// Stream the local clipboard's `mime_type` into `reply` for the server;
/// dropping `reply` marks the end. An error is sent as such. The server's
/// own offer is refused: reading it would fetch the item back from the
/// server, which would ask us for it again.
pub fn read_local(mime_type: String, reply: Sender<std::io::Result<Bytes>>) {
    glib::MainContext::default().spawn_local(async move {
        let Some(cb) = clipboard() else {
            return;
        };
        if holds_remote_offer(&cb) {
            let error = std::io::Error::other("clipboard holds the server's offer");
            let _ = reply.send(Err(error)).await;
            return;
        }
        // A text request may name a flavour the local source does not
        // advertise; any text flavour it does have will do.
        let available: Vec<String> = cb
            .formats()
            .mime_types()
            .iter()
            .map(|m| m.to_string())
            .collect();
        let Some(actual) = resolve_mime(&mime_type, &available) else {
            let error = std::io::Error::other(format!("clipboard has no {mime_type}"));
            let _ = reply.send(Err(error)).await;
            return;
        };
        let stream = match cb.read_future(&[actual], glib::Priority::DEFAULT).await {
            Ok((stream, _)) => stream,
            Err(e) => {
                let _ = reply.send(Err(std::io::Error::other(e.to_string()))).await;
                return;
            }
        };
        loop {
            match stream
                .read_bytes_future(CHUNK, glib::Priority::DEFAULT)
                .await
            {
                Ok(b) if b.is_empty() => break,
                Ok(b) => {
                    if reply.send(Ok(Bytes::copy_from_slice(&b))).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = reply.send(Err(std::io::Error::other(e.to_string()))).await;
                    break;
                }
            }
        }
        let _ = stream.close_future(glib::Priority::DEFAULT).await;
    });
}

/// Make the server's offer the local selection, or clear ours for an empty
/// offer. The offered mimes are fetched eagerly (see the module docs: a
/// lazy provider cannot be served from macOS's pasteboard fulfilment)
/// and set as one in-memory provider per mime.
pub fn set_remote_offer(
    tx: &UnboundedSender<ToWorker>,
    serial: u32,
    mime_types: Vec<String>,
    files: Vec<ClipboardFile>,
) {
    let Some(cb) = clipboard() else {
        return;
    };
    let mimes = local_mimes_for_offer(&mime_types, !files.is_empty());
    if mimes.is_empty() {
        if holds_remote_offer(&cb) {
            let _ = cb.set_content(gdk::ContentProvider::NONE);
        }
        return;
    }
    // A newer offer that races this fetch simply sets its own content
    // when it arrives; a fetch for a superseded serial is dropped.
    let latest = LATEST_OFFER.with(|l| {
        l.set(serial.max(l.get()));
        serial
    });
    let tx = tx.clone();
    glib::spawn_future_local(async move {
        let mut providers = Vec::new();
        for mime in &mimes {
            let Some(bytes) = fetch_mime(&tx, latest, mime.clone()).await else {
                continue;
            };
            providers.push(gdk::ContentProvider::for_bytes(
                mime,
                &glib::Bytes::from_owned(bytes.to_vec()),
            ));
        }
        if providers.is_empty() {
            return;
        }
        if LATEST_OFFER.with(|l| l.get()) != latest {
            return; // a newer offer arrived while fetching
        }
        let provider = if providers.len() == 1 {
            providers.pop().expect("checked non-empty")
        } else {
            gdk::ContentProvider::new_union(&providers)
        };
        MIRROR.with(|m| *m.borrow_mut() = Some(provider.clone()));
        if let Err(e) = cb.set_content(Some(&provider)) {
            tracing::warn!(error = %e, "clipboard mirror not set");
            MIRROR.with(|m| *m.borrow_mut() = None);
        }
    });
}

thread_local! {
    /// Serial of the newest offer the mirror is fetching or showing.
    static LATEST_OFFER: Cell<u32> = const { Cell::new(0) };
}

/// Fetch one offered mime from the server through the worker, `None`
/// when unreachable or larger than [`EAGER_MAX`].
async fn fetch_mime(tx: &UnboundedSender<ToWorker>, serial: u32, mime: String) -> Option<Bytes> {
    let (sink, mut rx) = tokio::sync::mpsc::channel::<Bytes>(2);
    let (result_tx, result_rx) = oneshot::channel();
    tx.send(ToWorker::Fetch {
        mime_type: mime,
        serial,
        sink,
        result: result_tx,
    })
    .ok()?;
    let mut out = Vec::new();
    while let Some(chunk) = rx.recv().await {
        out.extend_from_slice(&chunk);
        if out.len() > EAGER_MAX {
            tracing::warn!("offered clipboard item exceeds the eager-fetch cap; not mirrored");
            return None;
        }
    }
    match result_rx.await {
        Ok(Ok(())) => Some(Bytes::from(out)),
        _ => None,
    }
}

/// The transfer bars shown over the video while a paste from the server
/// takes a while: one row per job, with a cancel button. Empty rows are
/// removed, so the box is only visible while something is in flight.
pub struct TransferBars {
    list: gtk::Box,
    rows: RefCell<HashMap<u32, Row>>,
}

struct Row {
    root: gtk::Box,
    text: gtk::Label,
    bar: gtk::ProgressBar,
}

/// How long a failure stays on screen.
const FAILURE_LINGER: Duration = Duration::from_secs(6);

impl TransferBars {
    pub fn new() -> Self {
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::End)
            .visible(false)
            .css_classes(["transfers"])
            .build();
        Self {
            list,
            rows: RefCell::default(),
        }
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.list
    }

    /// Show the state of job `id`; `cancel` is called with the id when the
    /// user presses the row's button.
    pub fn update(self: &Rc<Self>, id: u32, p: &Progress, cancel: impl Fn(u32) + 'static) {
        match &p.state {
            State::Running => {
                let mut rows = self.rows.borrow_mut();
                let row = rows.entry(id).or_insert_with(|| self.add_row(id, cancel));
                row.text
                    .set_text(&format!("Pasting {}: {}", p.label, describe(p)));
                match p.percent() {
                    Some(pct) => row.bar.set_fraction(pct as f64 / 100.0),
                    None => row.bar.pulse(),
                }
            }
            State::Done | State::Cancelled => self.remove(id),
            State::Failed(e) => {
                let mut rows = self.rows.borrow_mut();
                let row = rows.entry(id).or_insert_with(|| self.add_row(id, cancel));
                row.text
                    .set_text(&format!("Paste of {} failed: {e}", p.label));
                row.bar.set_visible(false);
                drop(rows);
                let this = self.clone();
                glib::timeout_add_local_once(FAILURE_LINGER, move || this.remove(id));
            }
        }
    }

    fn add_row(&self, id: u32, cancel: impl Fn(u32) + 'static) -> Row {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .css_classes(["transfer"])
            .build();
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        let text = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let bar = gtk::ProgressBar::new();
        column.append(&text);
        column.append(&bar);
        let button = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Cancel")
            .valign(gtk::Align::Center)
            .build();
        button.connect_clicked(move |_| cancel(id));
        root.append(&column);
        root.append(&button);
        self.list.append(&root);
        self.list.set_visible(true);
        Row { root, text, bar }
    }

    fn remove(&self, id: u32) {
        let mut rows = self.rows.borrow_mut();
        if let Some(row) = rows.remove(&id) {
            self.list.remove(&row.root);
        }
        self.list.set_visible(!rows.is_empty());
    }
}
