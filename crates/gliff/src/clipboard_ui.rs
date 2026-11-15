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
use std::collections::{HashMap, HashSet};
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
    // Unit tests drive the offer routing without a display.
    if !gtk::is_initialized_main_thread() {
        return None;
    }
    gdk::Display::default().map(|d| d.clipboard())
}

thread_local! {
    /// The in-memory provider our last remote mirror set, so its own
    /// clipboard change is not echoed back to the server and a dead
    /// session's mirror can be dropped again.
    static MIRROR: RefCell<Option<gdk::ContentProvider>> = const { RefCell::new(None) };
}

/// Only called with a live clipboard, so unit tests never reach the
/// native pasteboard check.
fn sensitive_selection(mimes: &[String]) -> bool {
    #[cfg(target_os = "macos")]
    if concealed_pasteboard() {
        return true;
    }
    mimes.iter().any(|m| m == "x-kde-passwordManagerHint")
}

/// Whether the general pasteboard holds `org.nspasteboard.ConcealedType`,
/// the macOS password managers' marker. GTK reports that type only as
/// `application/octet-stream`, so it is asked of the pasteboard directly.
#[cfg(target_os = "macos")]
fn concealed_pasteboard() -> bool {
    use std::ffi::{c_char, c_void};
    use std::mem::transmute;
    type Id = *mut c_void;
    type Sel = *const c_void;
    // Declared as in cursor.rs, as a second signature would clash; each
    // call casts it to its method's own.
    type Msg = unsafe extern "C" fn(Id, Sel);
    #[link(name = "objc", kind = "dylib")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend(receiver: Id, sel: Sel);
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }
    // SAFETY: objc_msgSend is called through a pointer of each method's
    // exact signature (+generalPasteboard, -types, +stringWithUTF8String:
    // -> id; -containsObject: -> BOOL); a nil receiver answers nil or NO.
    // The objects autoreleased here are freed by the pool popped here.
    unsafe {
        let send_id = transmute::<Msg, unsafe extern "C" fn(Id, Sel) -> Id>(objc_msgSend);
        let send_cstr =
            transmute::<Msg, unsafe extern "C" fn(Id, Sel, *const c_char) -> Id>(objc_msgSend);
        let send_bool = transmute::<Msg, unsafe extern "C" fn(Id, Sel, Id) -> bool>(objc_msgSend);
        let pool = objc_autoreleasePoolPush();
        let pasteboard = send_id(
            objc_getClass(c"NSPasteboard".as_ptr()),
            sel_registerName(c"generalPasteboard".as_ptr()),
        );
        let types = send_id(pasteboard, sel_registerName(c"types".as_ptr()));
        let concealed = send_cstr(
            objc_getClass(c"NSString".as_ptr()),
            sel_registerName(c"stringWithUTF8String:".as_ptr()),
            c"org.nspasteboard.ConcealedType".as_ptr(),
        );
        let found = send_bool(
            types,
            sel_registerName(c"containsObject:".as_ptr()),
            concealed,
        );
        objc_autoreleasePoolPop(pool);
        found
    }
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

type Senders = Rc<dyn Fn() -> Vec<UnboundedSender<ToWorker>>>;

/// Report every change of the local clipboard to the workers. Each change
/// first revokes the previous local offer in every session `running` names.
/// A change to our own proxy provider stops there, and any other change is
/// then offered to the sessions `shown` names. The returned handle offers
/// the current selection again, for a session that has just been shown.
pub fn watch_local(
    running: impl Fn() -> Vec<UnboundedSender<ToWorker>> + 'static,
    shown: impl Fn() -> Vec<UnboundedSender<ToWorker>> + 'static,
) -> Option<LocalSelection> {
    let cb = clipboard()?;
    let watch = LocalSelection::new(running, shown);
    {
        let watch = watch.clone();
        cb.connect_changed(move |cb| watch.offer(cb));
    }
    Some(watch)
}

/// Offers the local selection to the sessions `shown` names. They are asked
/// when an offer is sent, not when the clipboard changes, so a slow file
/// listing goes to whichever session is shown by then.
#[derive(Clone)]
pub struct LocalSelection {
    running: Senders,
    shown: Senders,
    /// Bumped per offer so a slow file listing for a stale one is dropped,
    /// and a read for an offer the clipboard has since moved past is refused.
    selection_gen: Rc<Cell<u64>>,
}

impl LocalSelection {
    pub fn new(
        running: impl Fn() -> Vec<UnboundedSender<ToWorker>> + 'static,
        shown: impl Fn() -> Vec<UnboundedSender<ToWorker>> + 'static,
    ) -> Self {
        Self {
            running: Rc::new(running),
            shown: Rc::new(shown),
            selection_gen: Rc::new(Cell::new(0)),
        }
    }

    /// The generation of the latest offer. An offer is current only while
    /// this still matches the one it was sent with.
    pub fn generation(&self) -> u64 {
        self.selection_gen.get()
    }

    /// Offer the current local selection again. The clipboard reports only
    /// changes, so a session shown after the last copy would otherwise have
    /// nothing to paste. Nothing is revoked here: the session shown before
    /// was revoked by the switch, and the clipboard has not changed.
    pub fn reoffer(&self) {
        let gen = self.begin();
        if let Some(cb) = clipboard() {
            if !holds_remote_offer(&cb) {
                self.read_formats(&cb, gen);
            }
        }
    }

    /// The local clipboard changed: revoke the current offer in every
    /// running session and start a new offer. Returns its generation, or
    /// None when nothing is to be offered because the server's offer holds
    /// the clipboard or no session runs.
    pub fn changed(&self, holds_remote: bool) -> Option<u64> {
        let gen = self.begin();
        revoke_for_change(&(self.running)(), holds_remote).then_some(gen)
    }

    fn offer(&self, cb: &gdk::Clipboard) {
        // Only our own proxy for the server's offer must not echo back;
        // a copy from another gliff widget is a real local change.
        if let Some(gen) = self.changed(holds_remote_offer(cb)) {
            self.read_formats(cb, gen);
        }
    }

    /// Start a new offer, superseding any still being prepared.
    pub fn begin(&self) -> u64 {
        let gen = self.selection_gen.get() + 1;
        self.selection_gen.set(gen);
        gen
    }

    fn read_formats(&self, cb: &gdk::Clipboard, gen: u64) {
        if (self.shown)().is_empty() {
            return;
        }
        let watch = self.clone();
        let mimes: Vec<String> = cb
            .formats()
            .mime_types()
            .iter()
            .map(|m| m.to_string())
            .collect();
        let cb = cb.clone();
        glib::MainContext::default().spawn_local(async move {
            let sensitive = sensitive_selection(&mimes);
            let files = if !sensitive && offers_files(&mimes) {
                local_files(&cb, &mimes).await
            } else {
                LocalFiles::default()
            };
            let mime_types = if sensitive {
                Vec::new()
            } else {
                forwardable_mimes(&mimes)
            };
            watch.finish(gen, mime_types, files);
        });
    }

    /// Send offer `gen` once its files are listed, unless a newer offer has
    /// started since.
    pub fn finish(&self, gen: u64, mime_types: Vec<String>, files: LocalFiles) {
        if self.selection_gen.get() != gen {
            return;
        }
        for tx in (self.shown)() {
            let _ = tx.send(ToWorker::LocalOffer {
                mime_types: mime_types.clone(),
                files: files.clone(),
                generation: gen,
            });
        }
    }
}

/// Withdraw the current local offer from every session, and say whether the
/// change is a local selection to offer. The revocation comes first, before
/// the file listing, and also when the server's offer took the clipboard:
/// then no new offer replaces the old one, and its files would stay
/// readable by every session.
fn revoke_for_change(txs: &[UnboundedSender<ToWorker>], holds_remote: bool) -> bool {
    for tx in txs {
        let _ = tx.send(ToWorker::RevokeLocal);
    }
    !holds_remote && !txs.is_empty()
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
/// server, which would ask us for it again. `allowed` is asked when the
/// read starts: a request queued before a tab switch or a new copy must not
/// read what the clipboard holds now.
pub fn read_local(
    mime_type: String,
    reply: Sender<std::io::Result<Bytes>>,
    allowed: impl FnOnce() -> bool + 'static,
) {
    glib::MainContext::default().spawn_local(async move {
        if !allowed() {
            let error = std::io::Error::other("clipboard offer was withdrawn");
            let _ = reply.send(Err(error)).await;
            return;
        }
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
        if sensitive_selection(&available) {
            let _ = reply
                .send(Err(std::io::Error::other(
                    "clipboard selection is marked sensitive",
                )))
                .await;
            return;
        }
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
/// and set as one in-memory provider per mime. `still_shown` is checked
/// again once the fetch is done: the user may have switched to another
/// machine meanwhile, and only the shown one may take the clipboard.
pub fn set_remote_offer(
    tx: &UnboundedSender<ToWorker>,
    serial: u32,
    mime_types: Vec<String>,
    files: Vec<ClipboardFile>,
    still_shown: impl Fn() -> bool + 'static,
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
    // when it arrives; a superseded fetch is dropped. Counted locally:
    // server serials restart per bridge, so they can't be compared across
    // sessions or reconnects.
    let latest = LATEST_OFFER.with(|l| {
        let next = l.get().wrapping_add(1);
        l.set(next);
        next
    });
    let tx = tx.clone();
    glib::spawn_future_local(async move {
        let mut providers = Vec::new();
        for mime in &mimes {
            let Some(bytes) = fetch_mime(&tx, serial, mime.clone()).await else {
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
        if LATEST_OFFER.with(|l| l.get()) != latest || !still_shown() {
            return; // superseded, or no longer the shown machine
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
    /// Generation of the newest offer the mirror is fetching or showing.
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
    rows: RefCell<Bars<Row>>,
}

struct Row {
    root: gtk::Box,
    text: gtk::Label,
    bar: gtk::ProgressBar,
}

/// A row is keyed by the connection that ran its job and the job's id, as
/// every connection numbers its jobs from 1.
type BarKey = (u64, u32);

/// The rows of [`TransferBars`] by key, apart from the widgets so the
/// bookkeeping can be tested without a display.
struct Bars<R> {
    rows: HashMap<BarKey, R>,
    /// Rows the user closed whose job has not reported an end yet, so a
    /// report already on its way does not bring the row back.
    dismissed: HashSet<BarKey>,
}

impl<R> Default for Bars<R> {
    fn default() -> Self {
        Self {
            rows: HashMap::new(),
            dismissed: HashSet::new(),
        }
    }
}

impl<R> Bars<R> {
    /// The row to show a report in, added with `add` when there is none;
    /// `None` once the user closed it.
    fn row(&mut self, key: BarKey, add: impl FnOnce() -> R) -> Option<&mut R> {
        if self.dismissed.contains(&key) {
            return None;
        }
        Some(self.rows.entry(key).or_insert_with(add))
    }

    fn is_dismissed(&self, key: BarKey) -> bool {
        self.dismissed.contains(&key)
    }

    /// The job ended: its row goes, and so does any mark that it was closed.
    fn finish(&mut self, key: BarKey) -> Option<R> {
        self.dismissed.remove(&key);
        self.rows.remove(&key)
    }

    /// The user closed the row; it stays away until the job ends.
    fn dismiss(&mut self, key: BarKey) -> Option<R> {
        self.dismissed.insert(key);
        self.rows.remove(&key)
    }

    /// The connection ended, and with it every job it ran, reported or not.
    fn clear_connection(&mut self, conn: u64) -> Vec<R> {
        self.dismissed.retain(|&(c, _)| c != conn);
        let keys: Vec<BarKey> = self.rows.keys().filter(|k| k.0 == conn).copied().collect();
        keys.into_iter()
            .filter_map(|k| self.rows.remove(&k))
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
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

    /// Show the state of job `id` of connection `conn`; `cancel` is called
    /// with the id when the user closes the row, which removes it at once.
    pub fn update(
        self: &Rc<Self>,
        conn: u64,
        id: u32,
        p: &Progress,
        cancel: impl Fn(u32) + 'static,
    ) {
        let key = (conn, id);
        let mut rows = self.rows.borrow_mut();
        match &p.state {
            State::Running => {
                let Some(row) = rows.row(key, || self.add_row(key, cancel)) else {
                    return;
                };
                row.text
                    .set_text(&format!("Pasting {}: {}", p.label, describe(p)));
                match p.percent() {
                    Some(pct) => row.bar.set_fraction(pct as f64 / 100.0),
                    None => row.bar.pulse(),
                }
            }
            State::Done | State::Cancelled => {
                let row = rows.finish(key);
                drop(rows);
                self.drop_rows(row);
            }
            State::Failed(e) => {
                if rows.is_dismissed(key) {
                    rows.finish(key);
                    return;
                }
                let Some(row) = rows.row(key, || self.add_row(key, cancel)) else {
                    return;
                };
                row.text
                    .set_text(&format!("Paste of {} failed: {e}", p.label));
                row.bar.set_visible(false);
                drop(rows);
                let this = self.clone();
                glib::timeout_add_local_once(FAILURE_LINGER, move || {
                    let row = this.rows.borrow_mut().finish(key);
                    this.drop_rows(row);
                });
            }
        }
    }

    /// Remove the rows of a connection that ended. Its jobs died with it
    /// without a final report, so nothing else would.
    pub fn clear_connection(&self, conn: u64) {
        let rows = self.rows.borrow_mut().clear_connection(conn);
        self.drop_rows(rows);
    }

    fn add_row(self: &Rc<Self>, key: BarKey, cancel: impl Fn(u32) + 'static) -> Row {
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
        // The row goes whether or not the job is still there to cancel.
        let bars = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            cancel(key.1);
            if let Some(bars) = bars.upgrade() {
                let row = bars.rows.borrow_mut().dismiss(key);
                bars.drop_rows(row);
            }
        });
        root.append(&column);
        root.append(&button);
        self.list.append(&root);
        self.list.set_visible(true);
        Row { root, text, bar }
    }

    fn drop_rows(&self, rows: impl IntoIterator<Item = Row>) {
        for row in rows {
            self.list.remove(&row.root);
        }
        self.list.set_visible(!self.rows.borrow().is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(bars: &Bars<&'static str>) -> Vec<BarKey> {
        let mut keys: Vec<BarKey> = bars.rows.keys().copied().collect();
        keys.sort();
        keys
    }

    #[test]
    fn jobs_of_two_connections_keep_their_own_rows() {
        let mut bars = Bars::default();
        bars.row((1, 1), || "old");
        bars.row((2, 1), || "new");
        assert_eq!(shown(&bars), [(1, 1), (2, 1)]);
        assert_eq!(bars.row((2, 1), || "replaced").map(|r| *r), Some("new"));
        assert_eq!(bars.finish((2, 1)), Some("new"));
        assert_eq!(shown(&bars), [(1, 1)]);
    }

    #[test]
    fn an_ended_connection_takes_only_its_rows() {
        let mut bars = Bars::default();
        bars.row((1, 1), || "a");
        bars.row((1, 2), || "b");
        bars.row((2, 1), || "c");
        let mut gone = bars.clear_connection(1);
        gone.sort();
        assert_eq!(gone, ["a", "b"]);
        assert_eq!(shown(&bars), [(2, 1)]);
        assert!(bars.clear_connection(1).is_empty());
    }

    #[test]
    fn a_closed_row_stays_closed_until_its_job_ends() {
        let mut bars = Bars::default();
        bars.row((1, 1), || "a");
        assert_eq!(bars.dismiss((1, 1)), Some("a"));
        // A progress report already queued must not bring it back.
        assert!(bars.row((1, 1), || "again").is_none());
        assert!(bars.is_empty());
        assert_eq!(bars.finish((1, 1)), None);
        assert!(!bars.is_dismissed((1, 1)));
    }

    #[test]
    fn a_row_with_no_job_left_can_still_be_closed() {
        // The worker that ran the job is gone, so no report will follow.
        let mut bars = Bars::default();
        bars.row((1, 7), || "stale");
        assert_eq!(bars.dismiss((1, 7)), Some("stale"));
        assert!(bars.is_empty());
    }

    #[test]
    fn closing_marks_go_with_their_connection() {
        let mut bars = Bars::default();
        bars.row((1, 1), || "a");
        bars.row((2, 1), || "b");
        bars.dismiss((1, 1));
        bars.dismiss((2, 1));
        bars.clear_connection(1);
        assert!(!bars.is_dismissed((1, 1)));
        assert!(bars.is_dismissed((2, 1)));
    }

    #[test]
    fn every_session_is_revoked_when_the_servers_offer_takes_the_clipboard() {
        for holds_remote in [true, false] {
            let (a_tx, mut a_rx) = tokio::sync::mpsc::unbounded_channel();
            let (b_tx, mut b_rx) = tokio::sync::mpsc::unbounded_channel();
            let offer = revoke_for_change(&[a_tx, b_tx], holds_remote);
            assert_eq!(offer, !holds_remote);
            for rx in [&mut a_rx, &mut b_rx] {
                assert!(matches!(rx.try_recv(), Ok(ToWorker::RevokeLocal)));
                assert!(rx.try_recv().is_err());
            }
        }
        assert!(!revoke_for_change(&[], false));
    }
}
