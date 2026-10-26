//! gliff: a GTK4/libadwaita window that connects to a gliff server,
//! decodes the video, shows it, and forwards keyboard and pointer input.
//!
//! Decode runs on a worker thread (see `net`); this file is the UI.
//! Decoded frames arrive as GPU-tier images (a Metal texture read back
//! on macOS) or plain BGRA pixels.

mod clipboard;
mod clipboard_ui;
#[cfg(target_os = "macos")]
mod cursor;
#[cfg(target_os = "macos")]
mod glvideo;
#[cfg(target_os = "macos")]
mod kbd;
mod keymap;
mod net;
mod paintable;
mod recent;
mod theme;

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, sync_channel, Receiver};
use std::time::{Duration, Instant};

use adw::prelude::*;
use clap::Parser;
use gliff_proto::{Axis, ClientMsg};
use gliff_transport::SshTarget;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use net::Frame;
use net::{Endpoint, Picture, Status, Worker};
use recent::{Config, DisplayMode, DisplaySetting};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

/// A message plus optional trailing payload, sent from the UI to the worker.
type OutSender = UnboundedSender<clipboard::ToWorker>;

const APP_NAME: &str = "mac-gliff";

#[derive(Parser, Clone)]
#[command(
    name = APP_NAME,
    version,
    about = "Remote-desktop a Hyprland session over ssh"
)]
struct Cli {
    /// `user@host` to ssh to and spawn gliff-server. Without it the window
    /// opens with a tab for each machine connected to before, or with the
    /// address bar focused when there are none.
    host: Option<String>,
    /// Dev: connect directly to a `gliff-server --listen` address. Repeat it
    /// to open a tab for each.
    #[arg(long)]
    connect: Vec<String>,
    /// Remote gliff-server path.
    #[arg(long, default_value = "gliff-server")]
    server_bin: String,
    /// Create a private headless output on the remote, sized and scaled to
    /// this window, instead of mirroring the remote's focused screen.
    #[arg(long, conflicts_with = "output")]
    headless: bool,
    /// Mirror the named remote output (e.g. `DP-1`) instead of the focused one.
    #[arg(long)]
    output: Option<String>,
    /// Hotkey that releases captured shortcuts and hands the keyboard back to
    /// the local compositor. Forms: a chord like `shift+escape`, `ctrl+alt+q`
    /// or `super+escape`; `double-<key>` for a double-tap (e.g.
    /// `double-escape`); or `none` to disable. The screen recaptures when you
    /// click it again.
    #[arg(long, default_value = "shift+escape")]
    release_hotkey: String,
    /// Chord that opens the remote's launcher while the keyboard is
    /// captured, translated to super+space on the remote. On macOS this
    /// bridges the chords the system keeps from apps: Spotlight serves
    /// ⌘Space itself, and GTK's macOS backend drops ⌥Space (its NBSP
    /// character never becomes a key event) — ⌃Space arrives and stands
    /// in. `none` disables; the forms are as for `--release-hotkey`.
    #[arg(long, default_value_t = default_launcher_hotkey())]
    launcher_hotkey: String,
    /// Video pipeline: `gpu` (Vulkan compute + VA-API, falling back to the
    /// CPU when unavailable) or `cpu` (force OpenH264 on the CPU). Overrides
    /// the MAC_GLIFF_VIDEO environment variable.
    #[arg(long, value_parser = ["gpu", "cpu"])]
    video: Option<String>,
    /// Start with the remote's sound off. The header's speaker button turns
    /// it on and off, and the window remembers the choice.
    #[arg(long)]
    no_audio: bool,
}

/// The launcher chord ships on for macOS — on Linux the real super keys
/// reach the remote anyway, so the translation starts off. ⌥Space would
/// be the natural pick, but GTK's macOS backend drops the key event its
/// NBSP keyval rides on (the Option key mutates keyvals), so ⌃Space it
/// is: ctrl leaves the characters plain enough to arrive.
fn default_launcher_hotkey() -> String {
    if cfg!(target_os = "macos") {
        "ctrl+space".into()
    } else {
        "none".into()
    }
}

/// Everything the UI shares with its callbacks.
struct App {
    window: adw::ApplicationWindow,
    /// The video widget, upcast; input controllers attach to it and we
    /// measure it. On macOS it is a stack holding the picture the
    /// texture tiers draw into and the GLArea the Metal tier draws
    /// into; on Linux, the picture itself.
    video: gtk::Widget,
    /// What the picture shows: the latest frame at an integer scale.
    frame: paintable::FramePaintable,
    /// The Metal tier's zero-copy display, and the stack switching the
    /// tiers' widgets (macOS only).
    #[cfg(target_os = "macos")]
    gl: Rc<glvideo::GlVideo>,
    #[cfg(target_os = "macos")]
    tier_stack: gtk::Stack,
    stats: gtk::Label,
    /// Centred over the picture while the shown session is not streaming:
    /// connecting, reconnecting, an error, or no session at all.
    status: gtk::Label,
    /// A short-lived note near the bottom of the picture, such as how to
    /// release the captured shortcuts.
    hint: gtk::Label,
    /// Bumped by every hint, so a newer one is not hidden by an older timer.
    hint_generation: Cell<u64>,
    /// The capture hint shows once per window, not on every pointer entry.
    capture_hinted: Cell<bool>,
    /// A tab per machine, to the left of the address bar.
    tabs: gtk::Box,
    /// Expands into the address bar; hidden while the address bar shows.
    add_btn: gtk::Button,
    /// Holds the address bar, which shows only while adding a machine, or
    /// while there are no tabs at all.
    add_revealer: gtk::Revealer,
    /// The address bar: Enter connects to the machine typed.
    entry: gtk::Entry,
    transfers: Rc<clipboard_ui::TransferBars>,
    /// Every running connection, in the order they were opened.
    sessions: RefCell<Vec<Rc<Session>>>,
    /// The session the window shows and sends input to.
    active: RefCell<Option<Rc<Session>>>,
    /// Size of the video widget in device pixels, rounded down to even.
    view_size: Cell<(u32, u32)>,
    /// Plays the shown machine's sound while active.
    sound_btn: gtk::ToggleButton,
    /// Evdev codes currently held on the remote, so they can all be released
    /// when the keyboard is handed back to the local compositor.
    pressed_keys: RefCell<BTreeSet<u32>>,
    /// Set by the release hotkey: the pointer is over the picture but the
    /// keyboard stays local until the pointer leaves, or the picture is
    /// clicked.
    released: Cell<bool>,
    /// The pointer is over the video widget, per the motion controller's
    /// enter/leave; a headless session hides the local pointer for exactly
    /// this span.
    pointer_over_video: Cell<bool>,
    /// The `NSCursor` hide/unhide balance, so a transition never hides
    /// twice or unhides the user's own hide.
    cursor_hidden: Cell<bool>,
    config_path: PathBuf,
    cli: Cli,
    keymap: keymap::Keymap,
}

/// One connection to a machine. Every session keeps streaming while the
/// window shows another, so switching tabs is instant; only the shown one
/// gets input.
/// The frame a macOS session last showed: a GDK texture from the CPU
/// tier, or the Metal tier's IOSurface frame for the GLArea.
#[cfg(target_os = "macos")]
enum CachedFrame {
    Texture(gdk::Texture),
    Metal(Rc<gliff_mtl::compute::BgraFrame>),
}

struct Session {
    /// The tab's name: `user@host`, or the address of a dev `--connect`.
    name: String,
    endpoint: Endpoint,
    input_tx: RefCell<Option<OutSender>>,
    /// Bumped by every `start_session` and by `stop_session`, so pollers and
    /// delayed reconnects of an earlier worker can tell they are stale.
    generation: Cell<u64>,
    /// Consecutive failed connection attempts, reset on a successful connect.
    retries: Cell<u32>,
    /// Automatic reconnects have given up; selecting the tab tries again.
    gave_up: Cell<bool>,
    /// Recorded in the config on the first successful connect.
    remembered: Cell<bool>,
    /// The server is streaming: set by its Connected status, cleared when
    /// the connection drops.
    connected: Cell<bool>,
    /// The session's output is a private headless one, whose frames carry
    /// the remote cursor painted in; set by its Connected status.
    headless: Cell<bool>,
    /// Size of the stream the server is sending, from the last decoded frame.
    stream_size: Cell<(u32, u32)>,
    /// The full-quality fit size the stream is drawn into; equals the stream
    /// size unless the server reduced the resolution. From the last decoded
    /// frame, so pointer mapping always matches the picture on screen.
    stream_view: Cell<(u32, u32)>,
    /// The view from the last StreamConfig (which may not be painted yet);
    /// only the resize gate reads it.
    server_view: Cell<(u32, u32)>,
    /// The server's frame-rate ceiling, for the stats overlay.
    fps_cap: Cell<u32>,
    /// The remote output's scale: pointer coordinates go in physical / scale.
    stream_scale: Cell<f32>,
    /// The size last asked of the server, so a pending resize is not repeated.
    resize_requested: Cell<(u32, u32)>,
    /// The output zoom last asked of the decoder; a new worker starts at 1.
    zoom: Cell<u32>,
    /// The latest frame and its view, painted when the tab is shown.
    #[cfg(target_os = "macos")]
    last_frame: RefCell<Option<(CachedFrame, (u32, u32))>>,
    cursor: RefCell<Option<gdk::Cursor>>,
    status: RefCell<String>,
    stats: RefCell<String>,
}

impl Session {
    fn new(endpoint: Endpoint) -> Self {
        Self {
            name: machine_name(&endpoint).to_string(),
            endpoint,
            input_tx: RefCell::new(None),
            generation: Cell::new(0),
            retries: Cell::new(0),
            gave_up: Cell::new(false),
            remembered: Cell::new(false),
            headless: Cell::new(false),
            connected: Cell::new(false),
            stream_size: Cell::new((0, 0)),
            stream_view: Cell::new((0, 0)),
            server_view: Cell::new((0, 0)),
            fps_cap: Cell::new(0),
            stream_scale: Cell::new(1.0),
            resize_requested: Cell::new((0, 0)),
            zoom: Cell::new(1),
            last_frame: RefCell::new(None),
            cursor: RefCell::new(None),
            status: RefCell::new(String::new()),
            stats: RefCell::new(String::new()),
        }
    }
}

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    // GTK uses the program name as the Wayland app ID.
    glib::set_prgname(Some(APP_NAME));
    let app = adw::Application::builder().build();
    // The theme is per display, so it is set up once. The monitor lives in
    // this closure for the app's lifetime; dropping it would stop theme
    // updates.
    let theme_monitor = RefCell::new(None);
    app.connect_startup(move |_| {
        gtk::Window::set_default_icon_name(APP_NAME);
        *theme_monitor.borrow_mut() = theme::follow_omarchy_theme();
    });
    app.connect_activate(move |app| build_ui(app, &cli));
    // GTK owns argv parsing; we already parsed with clap, so pass none.
    let empty: Vec<String> = vec![];
    app.run_with_args(&empty)
}

/// The endpoints named on the command line, if any.
fn endpoints_from_cli(cli: &Cli) -> Vec<Endpoint> {
    if !cli.connect.is_empty() {
        return cli.connect.iter().cloned().map(Endpoint::Tcp).collect();
    }
    cli.host
        .iter()
        .map(|host| ssh_endpoint(cli, host))
        .collect()
}

fn ssh_endpoint(cli: &Cli, host: &str) -> Endpoint {
    let mut t = SshTarget::new(host.to_string());
    t.server_bin = cli.server_bin.clone();
    t.server_args = match (&cli.output, cli.headless) {
        (Some(name), _) => vec!["--output".into(), name.clone()],
        (None, true) => vec!["--headless".into()],
        (None, false) => vec!["--output".into(), "auto".into()],
    };
    Endpoint::Ssh(t)
}

/// A machine's endpoint under an explicit screen mapping: mirroring its
/// focused screen, or a private headless output that follows this window.
fn ssh_endpoint_with_mode(cli: &Cli, host: &str, mode: DisplayMode) -> Endpoint {
    let mut t = SshTarget::new(host.to_string());
    t.server_bin = cli.server_bin.clone();
    t.server_args = match mode {
        DisplayMode::Mirror => vec!["--output".into(), "auto".into()],
        DisplayMode::Window => vec!["--headless".into()],
    };
    Endpoint::Ssh(t)
}

/// The tab name for an endpoint.
fn machine_name(endpoint: &Endpoint) -> &str {
    match endpoint {
        Endpoint::Ssh(t) => t.host.as_str(),
        Endpoint::Tcp(addr) => addr.as_str(),
    }
}

/// Title in the `Document — App` form used by GNOME apps, so the machine
/// is what a task switcher shows first.
fn window_title(endpoint: &Endpoint) -> String {
    format!("{} — Gliff", machine_name(endpoint))
}

/// The header bar's window buttons on macOS. libadwaita ships only
/// monochrome circles, so paint the native traffic-light colours, grey
/// them out while the window is not focused, and reveal each glyph only
/// under the pointer, as the real title bar does.
#[cfg(target_os = "macos")]
const WINDOW_CONTROLS_CSS: &str = r#"
windowcontrols > button > image { color: transparent; }
windowcontrols > button.close > image { background-color: #ff5f57; }
windowcontrols > button.minimize > image { background-color: #febc2e; }
windowcontrols > button.maximize > image { background-color: #28c840; }
windowcontrols > button.close:hover > image { background-color: #e0443e; }
windowcontrols > button.minimize:hover > image { background-color: #dea123; }
windowcontrols > button.maximize:hover > image { background-color: #1aab29; }
windowcontrols > button.close:active > image { background-color: #cc3a32; }
windowcontrols > button.minimize:active > image { background-color: #c48f1e; }
windowcontrols > button.maximize:active > image { background-color: #179231; }
windowcontrols > button:hover > image,
windowcontrols > button:active > image { color: rgba(0, 0, 0, 0.55); }
windowcontrols > button:backdrop > image { background-color: alpha(currentColor, 0.3); }
"#;

/// The one window: a header with a tab per machine and the address bar, the
/// remote screen (black until connected) with any status over it.
fn build_ui(app: &adw::Application, cli: &Cli) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Gliff")
        .default_width(1280)
        .default_height(760)
        .build();

    let header = adw::HeaderBar::builder()
        .show_title(false)
        // On Linux closing is the compositor's job, and a stray click on an
        // X in the middle of a remote session is a bad surprise. A macOS
        // window is CSD: its traffic lights are the header bar's start
        // buttons, so they are shown there and nowhere else.
        .decoration_layout(if cfg!(target_os = "macos") {
            "close,minimize,maximize"
        } else {
            ""
        })
        .show_start_title_buttons(cfg!(target_os = "macos"))
        .build();
    let entry = gtk::Entry::builder()
        .placeholder_text("user@host")
        .width_chars(32)
        .build();
    let connect_btn = gtk::Button::builder()
        .icon_name(CONNECT_ICON)
        .tooltip_text("Connect")
        .build();
    let address_bar = gtk::Box::builder().css_classes(["linked"]).build();
    address_bar.append(&entry);
    address_bar.append(&connect_btn);
    let add_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideRight)
        .child(&address_bar)
        .build();
    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Connect to another machine")
        .css_classes(["flat"])
        .build();
    let fullscreen_btn = gtk::ToggleButton::builder()
        .icon_name("view-fullscreen-symbolic")
        .build();
    let stats_btn = gtk::ToggleButton::builder()
        .icon_name("dialog-information-symbolic")
        .tooltip_text("Show stats")
        .build();
    let sound_btn = gtk::ToggleButton::builder()
        .icon_name("audio-volume-high-symbolic")
        .tooltip_text("Play the remote's sound")
        .build();
    let tabs = gtk::Box::builder().spacing(6).build();
    let tabs_scroller = gtk::ScrolledWindow::builder()
        .child(&tabs)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .build();
    header.pack_start(&tabs_scroller);
    header.pack_start(&add_btn);
    header.pack_start(&add_revealer);
    header.pack_end(&fullscreen_btn);
    header.pack_end(&stats_btn);
    header.pack_end(&sound_btn);

    let stats = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .css_classes(["stats"])
        .build();
    stats_btn
        .bind_property("active", &stats, "visible")
        .sync_create()
        .build();
    let status = gtk::Label::builder()
        .label("Not connected")
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .can_target(false)
        .css_classes(["status-message"])
        .build();
    let hint = gtk::Label::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::End)
        .visible(false)
        .can_target(false)
        .css_classes(["hint"])
        .build();

    // The texture tier's display: a plain Picture given the whole
    // allocation (Fill); the paintable places the frame itself at an
    // integer scale, see paintable::layout. On macOS the Metal tier's
    // GLArea sits beside it in a stack and is shown when it has the
    // frame.
    let picture = gtk::Picture::builder()
        .hexpand(true)
        .vexpand(true)
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Fill)
        .css_classes(["video"])
        .build();
    let frame = paintable::FramePaintable::default();
    picture.set_paintable(Some(&frame));
    #[cfg(target_os = "macos")]
    let gl = glvideo::GlVideo::new();
    #[cfg(target_os = "macos")]
    let tier_stack = {
        let stack = gtk::Stack::new();
        stack.set_css_classes(&["video"]);
        stack.add_named(&picture, Some("texture"));
        stack.add_named(gl.widget(), Some("gl"));
        stack.set_visible_child_name("texture");
        stack
    };
    let video: gtk::Widget = {
        #[cfg(target_os = "macos")]
        {
            tier_stack.clone().upcast()
        }
    };

    let transfers = Rc::new(clipboard_ui::TransferBars::new());
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&video));
    overlay.add_overlay(&stats);
    overlay.add_overlay(transfers.widget());
    overlay.add_overlay(&status);
    overlay.add_overlay(&hint);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&header);
    content.append(&overlay);
    window.set_content(Some(&content));
    install_fullscreen_bars(
        &window,
        &content,
        &overlay,
        &header,
        &fullscreen_btn,
        &entry,
    );

    let ui = Rc::new(App {
        window: window.clone(),
        video: video.clone(),
        frame,
        #[cfg(target_os = "macos")]
        gl,
        #[cfg(target_os = "macos")]
        tier_stack,
        stats: stats.clone(),
        status: status.clone(),
        hint,
        hint_generation: Cell::new(0),
        capture_hinted: Cell::new(false),
        tabs,
        add_btn,
        add_revealer,
        entry: entry.clone(),
        transfers,
        sessions: RefCell::new(Vec::new()),
        active: RefCell::new(None),
        view_size: Cell::new((0, 0)),
        sound_btn: sound_btn.clone(),
        pressed_keys: RefCell::new(BTreeSet::new()),
        released: Cell::new(false),
        pointer_over_video: Cell::new(false),
        cursor_hidden: Cell::new(false),
        config_path: Config::default_path(),
        cli: cli.clone(),
        keymap: keymap::watch(),
    });

    let hotkey = Hotkey::parse(&cli.release_hotkey).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "invalid --release-hotkey; shortcut release disabled");
        Hotkey::None
    });
    let launcher = Hotkey::parse(&cli.launcher_hotkey).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "invalid --launcher-hotkey; no launcher translation");
        Hotkey::None
    });
    install_input_handlers(&ui, &video, &window, hotkey, launcher);
    #[cfg(target_os = "macos")]
    install_local_shortcuts(&ui);
    install_resize_handler(&ui);
    install_sound_button(&ui);
    install_address_bar(&ui, &connect_btn);
    refresh_tabs(&ui);

    // Fullscreen toggle.
    {
        let window = window.clone();
        fullscreen_btn.connect_toggled(move |b| {
            if b.is_active() {
                window.fullscreen();
            } else {
                window.unfullscreen();
            }
        });
    }

    let css = gtk::CssProvider::new();
    css.load_from_string(&format!(
        "{}{}",
        concat!(
        ".video { background: #000; }",
        ".status-message { color: rgba(255,255,255,0.7); font-size: 1.15em; margin: 24px; }",
        ".hint { background: rgba(0,0,0,0.7); color: #fff; padding: 6px 14px; border-radius: 999px; margin-bottom: 24px; }",
        ".stats { background: rgba(0,0,0,0.6); color: #fff; padding: 6px; margin: 6px; border-radius: 6px; font-family: monospace; }",
        ".transfers { margin: 6px; }",
        ".transfer { background: rgba(0,0,0,0.7); color: #fff; padding: 6px 8px; border-radius: 6px; }",
        // A tab is one pill: the name and its stop/forget icon carry no
        // backgrounds of their own, only the icon brightens under the pointer.
        ".machine-tab { border-radius: 8px; }",
        ".machine-tab:hover { background: alpha(currentColor, 0.07); }",
        ".machine-tab.active { background: alpha(currentColor, 0.14); }",
        ".machine-tab > .tab-label { padding: 5px 2px 5px 12px; font-weight: bold; }",
        ".machine-tab > button { background: none; border: none; outline: none; box-shadow: none; min-height: 0; }",
        ".machine-tab > button.tab-action { padding: 0; min-width: 22px; min-height: 22px; margin-right: 4px; color: alpha(currentColor, 0.55); -gtk-icon-size: 12px; }",
        ".machine-tab > button.tab-action:hover { color: currentColor; }",
        ".status-dot { min-width: 6px; min-height: 6px; border-radius: 50%; border: 1.5px solid alpha(currentColor, 0.6); }",
        ".status-dot.connected { background: var(--success-color); border-color: var(--success-color); }",
        ),
        WINDOW_CONTROLS_CSS
    ));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    // Right-click in the header otherwise opens GtkWindowHandle's fallback
    // window menu (Restore/Minimize/Maximize/Close) and claims the
    // sequence before a tab's own menu gesture can run; turning the
    // titlebar action off leaves the tab menu the only right-click there.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_titlebar_right_click(Some("none"));
    }

    // Offer every local clipboard change to the servers; our own proxy for
    // a server's selection does not count as a change.
    {
        let ui = ui.clone();
        clipboard_ui::watch_local(move || {
            ui.sessions
                .borrow()
                .iter()
                .filter_map(|s| s.input_tx.borrow().clone())
                .collect()
        });
    }

    window.present();
    // Reconnect the machines that were connected when the window last
    // closed, then any named on the command line, which is shown first.
    let config = Config::load(&ui.config_path);
    let from_cli = endpoints_from_cli(cli);
    // Command-line endpoints open first: an explicit --headless or
    // --output beats the machine's remembered display mode, which the
    // remembered loop below would otherwise win by opening first.
    for endpoint in &from_cli {
        open_session(&ui, endpoint.clone());
    }
    for machine in &config.open {
        if find_session(&ui, machine).is_some() {
            continue;
        }
        // The machine's last choice, remembered or not; there is no click
        // to ask at.
        let endpoint = match config.display_setting(machine).map(|s| s.mode) {
            Some(mode) => ssh_endpoint_with_mode(cli, machine, mode),
            None => ssh_endpoint(cli, machine),
        };
        open_session(&ui, endpoint);
    }
    // The address bar showed for the empty tabs; with sessions open it
    // folds into the +.
    show_address_bar(&ui, false);
    let first = from_cli
        .first()
        .map(|e| machine_name(e).to_string())
        .or(config.shown)
        .and_then(|name| find_session(&ui, &name))
        .or_else(|| ui.sessions.borrow().first().cloned());
    match first {
        Some(first) => show_session(&ui, &first),
        None if ui.tabs.first_child().is_none() => {
            entry.grab_focus();
        }
        None => {}
    }
}

const CONNECT_ICON: &str = "go-next-symbolic";
/// Only GTK's builtin icon set is guaranteed on macOS (no Adwaita icon
/// theme ships with the app): pick from it or the button shows the
/// broken-image placeholder. The close X covers both stop and forget.
const STOP_ICON: &str = "window-close-symbolic";
const FORGET_ICON: &str = "window-close-symbolic";

/// Wire the address bar: the + expands it, Enter connects to what was
/// typed, and Escape, or leaving it empty, folds it back into the +.
fn install_address_bar(ui: &Rc<App>, connect_btn: &gtk::Button) {
    {
        let ui = ui.clone();
        ui.entry
            .clone()
            .connect_activate(move |_| connect_from_address_bar(&ui));
    }
    {
        let ui = ui.clone();
        connect_btn.connect_clicked(move |_| connect_from_address_bar(&ui));
    }
    {
        let ui = ui.clone();
        ui.add_btn.clone().connect_clicked(move |_| {
            show_address_bar(&ui, true);
            ui.entry.grab_focus();
        });
    }

    let focus = gtk::EventControllerFocus::new();
    {
        let ui = ui.clone();
        // Focus may be moving to the connect button, so decide once the new
        // focus is known.
        focus.connect_leave(move |_| {
            let ui = ui.clone();
            glib::idle_add_local_once(move || {
                let in_bar = gtk::prelude::GtkWindowExt::focus(&ui.window)
                    .is_some_and(|w| w.is_ancestor(&ui.add_revealer));
                if !in_bar && ui.entry.text().trim().is_empty() {
                    show_address_bar(&ui, false);
                }
            });
        });
    }
    ui.entry.add_controller(focus);

    let keys = gtk::EventControllerKey::new();
    {
        let ui = ui.clone();
        keys.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::Escape {
                cancel_address_edit(&ui);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    ui.entry.add_controller(keys);
}

/// Show the address bar in place of the +, or fold it back. With no tabs
/// the address bar is all there is, so it stays.
fn show_address_bar(ui: &App, show: bool) {
    let show = show || ui.tabs.first_child().is_none();
    ui.add_revealer.set_reveal_child(show);
    ui.add_btn.set_visible(!show);
    // A folded address bar must not keep the focus GTK gave it when the
    // window opened, or hovering the picture would never capture.
    if !show && address_bar_in_use(&ui.window, &ui.entry) {
        gtk::prelude::GtkWindowExt::set_focus(&ui.window, gtk::Widget::NONE);
    }
}

/// Connect to the machine in the address bar, or switch to its tab when it
/// is already connected.
fn connect_from_address_bar(ui: &Rc<App>) {
    let machine = ui.entry.text().trim().to_string();
    if machine.is_empty() {
        return;
    }
    ui.entry.set_text("");
    match find_session(ui, &machine) {
        Some(s) => show_session(ui, &s),
        None => connect_ssh(ui, &machine),
    }
    show_address_bar(ui, false);
    if active(ui).is_some() {
        ui.video.grab_focus();
    }
}

/// Connect to `machine` over ssh. The command line's own `--headless` or
/// `--output` fixes the mapping outright; a remembered choice is used
/// without asking; anything else asks first and connects once answered.
fn connect_ssh(ui: &Rc<App>, machine: &str) {
    if ui.cli.headless || ui.cli.output.is_some() {
        open_session(ui, ssh_endpoint(&ui.cli, machine));
        return;
    }
    let remembered = Config::load(&ui.config_path)
        .display_setting(machine)
        .filter(|s| s.remember);
    match remembered {
        Some(setting) => open_session(ui, ssh_endpoint_with_mode(&ui.cli, machine, setting.mode)),
        None => ask_display_mode(ui, machine),
    }
}

/// The connect dialog: how the remote screen should appear here, and
/// whether to remember the choice for this machine. Connect opens the
/// session under it.
fn ask_display_mode(ui: &Rc<App>, machine: &str) {
    let stored = Config::load(&ui.config_path).display_setting(machine);
    let preselect = stored.map_or(DisplayMode::Mirror, |s| s.mode);

    let (mirror, mirror_row) = choice_row(
        "Mirror the remote screen",
        "Show the remote's own display, at its own resolution.",
    );
    let (window, window_row) = choice_row(
        "Fit the remote screen to this window",
        "A private remote screen that resizes with this window.",
    );
    window.set_group(Some(&mirror));
    match preselect {
        DisplayMode::Mirror => mirror.set_active(true),
        DisplayMode::Window => window.set_active(true),
    }
    let remember = gtk::CheckButton::with_label("Remember this choice for this machine");
    remember.set_active(stored.is_some_and(|s| s.remember));
    let choices = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(6)
        .build();
    choices.append(&mirror_row);
    choices.append(&window_row);
    choices.append(&remember);

    let dialog = adw::AlertDialog::new(
        Some(&format!("Connect to {machine}")),
        Some("How should the remote screen appear?"),
    );
    dialog.set_extra_child(Some(&choices));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("connect", "Connect");
    dialog.set_response_appearance("connect", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("connect"));
    dialog.set_close_response("cancel");
    {
        let ui = ui.clone();
        let machine = machine.to_string();
        dialog.connect_response(None, move |_, response| {
            if response != "connect" {
                return;
            }
            let mode = if window.is_active() {
                DisplayMode::Window
            } else {
                DisplayMode::Mirror
            };
            let mut config = Config::load(&ui.config_path);
            config.set_display_setting(
                &machine,
                DisplaySetting {
                    mode,
                    remember: remember.is_active(),
                },
            );
            save_config(&ui, &config);
            open_session(&ui, ssh_endpoint_with_mode(&ui.cli, &machine, mode));
        });
    }
    dialog.present(Some(&ui.window));
}

/// One radio row of the connect dialog: the choice and a dim caption.
fn choice_row(title: &str, caption: &str) -> (gtk::CheckButton, gtk::Box) {
    let check = gtk::CheckButton::builder()
        .valign(gtk::Align::Start)
        .build();
    let title = gtk::Label::builder()
        .label(title)
        .halign(gtk::Align::Start)
        .build();
    let caption = gtk::Label::builder()
        .label(caption)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    text.append(&title);
    text.append(&caption);
    let row = gtk::Box::builder().spacing(8).build();
    row.append(&check);
    row.append(&text);
    (check, row)
}

/// Give up the edit: empty the address bar, fold it back into the +, and
/// hand focus back to the remote screen when there is one.
fn cancel_address_edit(ui: &App) {
    ui.entry.set_text("");
    show_address_bar(ui, false);
    if active(ui).is_some() {
        ui.video.grab_focus();
    } else if ui.add_revealer.reveals_child() {
        ui.entry.grab_focus();
    } else {
        gtk::prelude::GtkWindowExt::set_focus(&ui.window, gtk::Widget::NONE);
    }
}

fn active(ui: &App) -> Option<Rc<Session>> {
    ui.active.borrow().clone()
}

fn is_active(ui: &App, s: &Rc<Session>) -> bool {
    ui.active
        .borrow()
        .as_ref()
        .is_some_and(|a| Rc::ptr_eq(a, s))
}

fn find_session(ui: &App, name: &str) -> Option<Rc<Session>> {
    ui.sessions
        .borrow()
        .iter()
        .find(|s| s.name == name)
        .cloned()
}

/// The tabs, in order: the remembered machines as arranged, then any session
/// not remembered yet (a first connect still in progress, or a dev
/// `--connect`).
fn tab_names(ui: &App) -> Vec<String> {
    let mut names = Config::load(&ui.config_path).machines;
    for s in ui.sessions.borrow().iter() {
        if !names.contains(&s.name) {
            names.push(s.name.clone());
        }
    }
    names
}

/// Rebuild the tabs from the config and the running sessions.
fn refresh_tabs(ui: &Rc<App>) {
    while let Some(tab) = ui.tabs.first_child() {
        ui.tabs.remove(&tab);
    }
    for name in tab_names(ui) {
        ui.tabs.append(&machine_tab(ui, &name));
    }
    // An empty scroller still claims its minimum width, which would push
    // the address bar away from where the first tab starts.
    let has_tabs = ui.tabs.first_child().is_some();
    if let Some(scroller) = ui.tabs.ancestor(gtk::ScrolledWindow::static_type()) {
        scroller.set_visible(has_tabs);
    }
    if !has_tabs {
        show_address_bar(ui, true);
    }
}

/// A machine's tab: its name, which shows that machine, and a button that
/// appears on hover: stop for a running session, forget for the rest. Tabs
/// can be dragged into a new order.
fn machine_tab(ui: &Rc<App>, name: &str) -> gtk::Box {
    let session = find_session(ui, name);
    let running = session.is_some();
    // The name is a plain label and the whole tab takes the click, so no
    // part of a tab takes keyboard focus and grows a focus ring.
    let label = gtk::Label::builder()
        .label(name)
        .css_classes(["tab-label"])
        .build();
    if !running {
        label.add_css_class("dim-label");
    }
    // The slot on the right shows whether the machine is connected, and
    // turns into its button while the pointer is on it.
    let connected = session.as_ref().is_some_and(|s| s.connected.get());
    let status_tip = if connected {
        "Connected"
    } else {
        "Not connected"
    };
    let action_tip = if running { "Disconnect" } else { "Forget" };
    let dot = gtk::Box::builder()
        .css_classes(["status-dot"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    if connected {
        dot.add_css_class("connected");
    }
    let slot = gtk::Stack::new();
    slot.add_named(&dot, Some("status"));
    slot.add_named(
        &gtk::Image::from_icon_name(if running { STOP_ICON } else { FORGET_ICON }),
        Some("action"),
    );
    let action = gtk::Button::builder()
        .child(&slot)
        .tooltip_text(status_tip)
        .css_classes(["tab-action"])
        .valign(gtk::Align::Center)
        .focusable(false)
        .build();
    let tab = gtk::Box::builder().css_classes(["machine-tab"]).build();
    if session.as_ref().is_some_and(|s| is_active(ui, s)) {
        tab.add_css_class("active");
    }
    tab.append(&label);
    tab.append(&action);

    // The dot turns into its button only under the pointer, so a click on
    // it always lands on what it shows.
    let hover = gtk::EventControllerMotion::new();
    {
        let slot = slot.clone();
        hover.connect_enter(move |c, _, _| {
            slot.set_visible_child_name("action");
            if let Some(button) = c.widget() {
                button.set_tooltip_text(Some(action_tip));
            }
        });
    }
    {
        let slot = slot.clone();
        hover.connect_leave(move |c| {
            slot.set_visible_child_name("status");
            if let Some(button) = c.widget() {
                button.set_tooltip_text(Some(status_tip));
            }
        });
    }
    action.add_controller(hover);

    // The handlers rebuild the tabs, so they run once the click is done
    // with the widget that received it. The action button claims its own
    // clicks, so this sees only the rest of the tab.
    let click = gtk::GestureClick::builder().button(1).build();
    {
        let ui = ui.clone();
        let name = name.to_string();
        click.connect_released(move |_, _, _, _| {
            let (ui, name) = (ui.clone(), name.clone());
            glib::idle_add_local_once(move || select_tab(&ui, &name));
        });
    }
    tab.add_controller(click);
    {
        let ui = ui.clone();
        let name = name.to_string();
        action.connect_clicked(move |_| {
            let (ui, name) = (ui.clone(), name.clone());
            glib::idle_add_local_once(move || {
                if running {
                    stop_session(&ui, &name);
                } else {
                    forget_machine(&ui, &name);
                }
            });
        });
    }

    // Drag to reorder. The source runs in the capture phase so a drag
    // starting on the name wins over the button's click and the header's
    // window move.
    let drag = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    {
        let name = name.to_string();
        drag.connect_prepare(move |source, x, y| {
            if let Some(tab) = source.widget() {
                let icon = gtk::WidgetPaintable::new(Some(&tab));
                source.set_icon(Some(&icon), x as i32, y as i32);
            }
            Some(gdk::ContentProvider::for_value(&name.to_value()))
        });
    }
    tab.add_controller(drag);

    let drop = gtk::DropTarget::new(String::static_type(), gdk::DragAction::MOVE);
    {
        let ui = ui.clone();
        let name = name.to_string();
        drop.connect_drop(move |target, value, x, _| {
            let Ok(dragged) = value.get::<String>() else {
                return false;
            };
            let after = target.widget().is_some_and(|w| x > w.width() as f64 / 2.0);
            let (ui, name) = (ui.clone(), name.clone());
            glib::idle_add_local_once(move || move_tab(&ui, &dragged, &name, after));
            true
        });
    }
    tab.add_controller(drop);

    // Right-click offers the machine's screen mapping; dev TCP sessions
    // have none, and a machine with no session is an ssh one by
    // construction.
    if session
        .as_ref()
        .is_none_or(|s| matches!(s.endpoint, Endpoint::Ssh(_)))
    {
        install_tab_menu(ui, &tab, name);
    }
    tab
}

/// Record `mode` for `machine` and make it true now: a running ssh
/// session reconnects under it — the server is spawned with the mode's
/// argument — and a stopped machine takes it on the next connect.
fn set_display_mode(ui: &Rc<App>, name: &str, mode: DisplayMode) {
    let mut config = Config::load(&ui.config_path);
    let remember = config.display_setting(name).is_some_and(|s| s.remember);
    config.set_display_setting(name, DisplaySetting { mode, remember });
    save_config(ui, &config);
    // A session whose server already runs this mode needs no restart.
    let restart = find_session(ui, name)
        .as_ref()
        .and_then(|s| session_mode(s))
        .is_some_and(|current| current != mode);
    if restart {
        stop_session(ui, name);
        open_session(ui, ssh_endpoint_with_mode(&ui.cli, name, mode));
    } else {
        refresh_tabs(ui);
    }
}

/// The screen mapping a session's server was spawned under; a dev TCP
/// session has none.
fn session_mode(s: &Session) -> Option<DisplayMode> {
    match &s.endpoint {
        Endpoint::Ssh(t) if t.server_args.iter().any(|a| a == "--headless") => {
            Some(DisplayMode::Window)
        }
        Endpoint::Ssh(_) => Some(DisplayMode::Mirror),
        Endpoint::Tcp(_) => None,
    }
}

/// The tab's right-click menu: the machine's screen mapping, and whether
/// to ask for it on every connect instead of using the remembered choice.
fn install_tab_menu(ui: &Rc<App>, tab: &gtk::Box, name: &str) {
    let stored = Config::load(&ui.config_path).display_setting(name);
    let mode = stored.map_or(DisplayMode::Mirror, |s| s.mode);
    let ask = stored.is_none_or(|s| !s.remember);

    let mapping = gio::Menu::new();
    for (label, m) in [
        ("Mirror the remote screen", DisplayMode::Mirror),
        ("Fit the remote screen to this window", DisplayMode::Window),
    ] {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(
            Some("tab.display-mode"),
            Some(&glib::Variant::from(mode_arg(m))),
        );
        mapping.append_item(&item);
    }
    let asking = gio::Menu::new();
    let ask_item = gio::MenuItem::new(Some("Ask before connecting"), None);
    ask_item.set_action_and_target_value(Some("tab.ask"), None);
    asking.append_item(&ask_item);
    let menu = gio::Menu::new();
    menu.append_section(None, &mapping);
    menu.append_section(None, &asking);

    let mode_action = gio::SimpleAction::new_stateful(
        "display-mode",
        Some(glib::VariantTy::STRING),
        &mode_arg(mode).to_variant(),
    );
    {
        let ui = ui.clone();
        let name = name.to_string();
        mode_action.connect_activate(move |action, target| {
            let mode = target.and_then(|v| v.str()).and_then(mode_from_arg);
            if let Some(mode) = mode {
                action.set_state(&mode_arg(mode).to_variant());
                set_display_mode(&ui, &name, mode);
            }
        });
    }
    let ask_action = gio::SimpleAction::new_stateful("ask", None, &ask.to_variant());
    {
        let ui = ui.clone();
        let name = name.to_string();
        ask_action.connect_activate(move |action, _| {
            let ask = !action.state().and_then(|v| v.get::<bool>()).unwrap_or(true);
            action.set_state(&ask.to_variant());
            let mode = Config::load(&ui.config_path)
                .display_setting(&name)
                .map_or(DisplayMode::Mirror, |s| s.mode);
            let mut config = Config::load(&ui.config_path);
            config.set_display_setting(
                &name,
                DisplaySetting {
                    mode,
                    remember: !ask,
                },
            );
            save_config(&ui, &config);
            refresh_tabs(&ui);
        });
    }
    let actions = gio::SimpleActionGroup::new();
    actions.add_action(&mode_action);
    actions.add_action(&ask_action);
    tab.insert_action_group("tab", Some(&actions));

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_parent(tab);
    let click = gtk::GestureClick::builder().button(3).build();
    {
        let popover = popover.clone();
        click.connect_released(move |_, _, _, _| popover.popup());
    }
    tab.add_controller(click);
}

fn mode_arg(mode: DisplayMode) -> &'static str {
    match mode {
        DisplayMode::Mirror => "mirror",
        DisplayMode::Window => "window",
    }
}

fn mode_from_arg(s: &str) -> Option<DisplayMode> {
    match s {
        "mirror" => Some(DisplayMode::Mirror),
        "window" => Some(DisplayMode::Window),
        _ => None,
    }
}

/// Show a machine's tab: switch to its running session, retrying one whose
/// reconnects gave up, or connect to it.
fn select_tab(ui: &Rc<App>, name: &str) {
    match find_session(ui, name) {
        Some(s) => {
            if s.gave_up.get() {
                s.retries.set(0);
                s.gave_up.set(false);
                start_session(ui.clone(), s.clone());
            }
            show_session(ui, &s);
        }
        None => connect_ssh(ui, name),
    }
}

/// Connect to `endpoint` in a new session and show it.
fn open_session(ui: &Rc<App>, endpoint: Endpoint) {
    let s = Rc::new(Session::new(endpoint));
    ui.sessions.borrow_mut().push(s.clone());
    start_session(ui.clone(), s.clone());
    show_session(ui, &s);
}

/// Make `s` the session the window shows and sends input to.
fn show_session(ui: &Rc<App>, s: &Rc<Session>) {
    if !is_active(ui, s) {
        release_pressed_keys(ui);
        *ui.active.borrow_mut() = Some(s.clone());
        if matches!(s.endpoint, Endpoint::Ssh(_)) {
            let mut config = Config::load(&ui.config_path);
            config.shown = Some(s.name.clone());
            save_config(ui, &config);
        }
    }
    ui.window.set_title(Some(&window_title(&s.endpoint)));
    show_status(ui, &s.status.borrow());
    ui.stats.set_text(&s.stats.borrow());
    #[cfg(target_os = "macos")]
    match &*s.last_frame.borrow() {
        Some((CachedFrame::Metal(f), view)) => {
            ui.gl.set_frame(f.clone(), ui.video.scale_factor(), *view);
            ui.tier_stack.set_visible_child_name("gl");
        }
        Some((CachedFrame::Texture(texture), view)) => {
            ui.frame
                .set_frame(texture.clone(), ui.video.scale_factor(), *view);
            ui.tier_stack.set_visible_child_name("texture");
        }
        None => {
            ui.frame.clear();
            ui.gl.clear();
        }
    }
    ui.video.set_cursor(s.cursor.borrow().as_ref());
    // Switching between a headless and a mirror session flips the hiding.
    update_cursor_hidden(ui);
    // A background session kept the size the window had when it was last
    // shown.
    request_resize(ui);
    request_zoom(ui);
    update_sound(ui);
    refresh_tabs(ui);
}

/// Show no session: a black screen, as on a fresh window.
fn show_nothing(ui: &Rc<App>) {
    release_pressed_keys(ui);
    ui.active.borrow_mut().take();
    ui.window.set_title(Some("Gliff"));
    show_status(ui, "Not connected");
    ui.stats.set_text("");
    ui.frame.clear();
    #[cfg(target_os = "macos")]
    ui.gl.clear();
    ui.video.set_cursor(None);
    update_cursor_hidden(ui);
    update_sound(ui);
    refresh_tabs(ui);
}

/// End a machine's session. When it was the one shown, the window moves to
/// the nearest running tab, to the right first.
fn stop_session(ui: &Rc<App>, name: &str) {
    let Some(s) = find_session(ui, name) else {
        return;
    };
    let was_active = is_active(ui, &s);
    if was_active {
        release_pressed_keys(ui);
    }
    // Bumping the generation stops its pollers and pending reconnects, and
    // dropping the sender closes the worker's input, which ends it.
    s.generation.set(s.generation.get() + 1);
    s.input_tx.borrow_mut().take();
    s.last_frame.borrow_mut().take();
    // A machine stopped by hand stays stopped when the window reopens.
    if matches!(s.endpoint, Endpoint::Ssh(_)) {
        let mut config = Config::load(&ui.config_path);
        config.set_open(&s.name, false);
        save_config(ui, &config);
    }
    let names = tab_names(ui);
    ui.sessions.borrow_mut().retain(|o| !Rc::ptr_eq(o, &s));
    if !was_active {
        refresh_tabs(ui);
        return;
    }
    let at = names.iter().position(|n| *n == s.name).unwrap_or(0);
    let next = names[at + 1..]
        .iter()
        .chain(names[..at].iter().rev())
        .find_map(|n| find_session(ui, n));
    match next {
        Some(next) => show_session(ui, &next),
        None => show_nothing(ui),
    }
}

/// Drop a machine that is not running from the remembered list.
fn forget_machine(ui: &Rc<App>, name: &str) {
    let mut config = Config::load(&ui.config_path);
    config.forget(name);
    save_config(ui, &config);
    refresh_tabs(ui);
}

/// Move the dragged tab next to the one it was dropped on. Only remembered
/// machines have a place to keep.
fn move_tab(ui: &Rc<App>, dragged: &str, target: &str, after: bool) {
    let mut config = Config::load(&ui.config_path);
    if config.move_machine(dragged, target, after) {
        save_config(ui, &config);
        refresh_tabs(ui);
    }
}

/// The sound button: on plays the shown machine's sound. `--no-audio` starts
/// it off; the choice is kept in the config.
fn install_sound_button(ui: &Rc<App>) {
    let on = !ui.cli.no_audio && !Config::load(&ui.config_path).muted;
    set_sound_icon(&ui.sound_btn, on);
    ui.sound_btn.set_active(on);
    let weak = Rc::downgrade(ui);
    ui.sound_btn.connect_toggled(move |btn| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        set_sound_icon(btn, btn.is_active());
        let mut config = Config::load(&ui.config_path);
        config.muted = !btn.is_active();
        save_config(&ui, &config);
        update_sound(&ui);
    });
}

fn set_sound_icon(btn: &gtk::ToggleButton, on: bool) {
    btn.set_icon_name(if on {
        "audio-volume-high-symbolic"
    } else {
        "audio-volume-muted-symbolic"
    });
    btn.set_tooltip_text(Some(if on {
        "Mute the remote's sound"
    } else {
        "Play the remote's sound"
    }));
}

/// Only the shown session plays sound, and only while the button is on;
/// every other session's server stops capturing it.
fn update_sound(ui: &App) {
    let on = ui.sound_btn.is_active();
    for s in ui.sessions.borrow().iter() {
        if let Some(tx) = s.input_tx.borrow().as_ref() {
            let _ = tx.send(clipboard::ToWorker::Audio(on && is_active(ui, s)));
        }
    }
}

fn save_config(ui: &App, config: &Config) {
    if let Err(e) = config.save(&ui.config_path) {
        tracing::warn!(path = %ui.config_path.display(), error = %e, "cannot save config");
    }
}

/// Set a session's status, showing it when the session is. A streaming
/// session has none.
fn set_status(ui: &App, s: &Rc<Session>, text: &str) {
    *s.status.borrow_mut() = text.to_string();
    if is_active(ui, s) {
        show_status(ui, text);
    }
}

fn show_status(ui: &App, text: &str) {
    ui.status.set_text(text);
    ui.status.set_visible(!text.is_empty());
}

/// Show a note near the bottom of the picture for a few seconds.
fn show_hint(ui: &Rc<App>, text: &str) {
    let generation = ui.hint_generation.get() + 1;
    ui.hint_generation.set(generation);
    ui.hint.set_text(text);
    ui.hint.set_visible(true);
    let ui = ui.clone();
    glib::timeout_add_local_once(Duration::from_millis(2500), move || {
        if ui.hint_generation.get() == generation {
            ui.hint.set_visible(false);
        }
    });
}

fn set_stats(ui: &App, s: &Rc<Session>, text: &str) {
    *s.stats.borrow_mut() = text.to_string();
    if is_active(ui, s) {
        ui.stats.set_text(text);
    }
}

/// Go back to a black screen with no stream geometry.
fn show_disconnected(ui: &Rc<App>, s: &Rc<Session>) {
    if s.connected.replace(false) {
        refresh_tabs(ui);
    }
    // The shown session's clipboard proxy holds unreachable content
    // now; a paste must not ask it for data (on macOS the pasteboard
    // fulfilment runs a nested main loop).
    if is_active(ui, s) {
        clipboard_ui::drop_remote_offer();
    }
    s.last_frame.borrow_mut().take();
    s.stream_size.set((0, 0));
    s.stream_view.set((0, 0));
    s.server_view.set((0, 0));
    s.cursor.borrow_mut().take();
    // The frames carrying the painted-in cursor are gone; the local
    // pointer shows again until the reconnect says headless once more.
    s.headless.set(false);
    set_stats(ui, s, "");
    if is_active(ui, s) {
        ui.frame.clear();
        #[cfg(target_os = "macos")]
        ui.gl.clear();
        ui.video.set_cursor(None);
        update_cursor_hidden(ui);
    }
}

/// Start a worker for the session. Each call is a new generation; pollers
/// and reconnects from an older one stop themselves.
fn start_session(ui: Rc<App>, s: Rc<Session>) {
    let (frame_tx, frame_rx) = sync_channel::<Picture>(2);
    let (status_tx, status_rx) = channel::<Status>();
    let (input_tx, input_rx) = unbounded_channel::<clipboard::ToWorker>();
    // Replacing the sender closes the old worker's input, which ends it.
    *s.input_tx.borrow_mut() = Some(input_tx);
    s.zoom.set(1);
    s.resize_requested.set((0, 0));
    let generation = s.generation.get() + 1;
    s.generation.set(generation);
    set_status(&ui, &s, "Connecting…");

    let endpoint = s.endpoint.clone();
    let video = gliff_sw::VideoMode::resolve(ui.cli.video.as_deref());
    let keymap = ui.keymap.clone();
    std::thread::Builder::new()
        .name("gliff-net".into())
        .spawn(move || {
            Worker {
                endpoint,
                video,
                frames: frame_tx,
                status: status_tx,
                input: input_rx,
                keymap,
            }
            .run();
        })
        .expect("spawn network thread");

    update_sound(&ui);
    poll_frames(ui.clone(), s.clone(), frame_rx, generation);
    poll_status(ui, s, status_rx, generation);
}

const MAX_RETRIES: u32 = 5;

/// Add the session's machine to the remembered list, and to the ones to
/// reconnect when the window opens again, once per session. The connected
/// status repeats on every stream reconfigure, such as a resize.
fn remember_machine(ui: &Rc<App>, s: &Session) {
    if !matches!(s.endpoint, Endpoint::Ssh(_)) || s.remembered.replace(true) {
        return;
    }
    let mut config = Config::load(&ui.config_path);
    config.remember(&s.name);
    config.set_open(&s.name, true);
    save_config(ui, &config);
    refresh_tabs(ui);
}

/// Schedule a reconnect after a short delay, unless we have exhausted retries.
fn schedule_reconnect(ui: Rc<App>, s: Rc<Session>, generation: u64) {
    let n = s.retries.get() + 1;
    s.retries.set(n);
    if n > MAX_RETRIES {
        set_status(&ui, &s, "Disconnected — click the tab to reconnect");
        s.gave_up.set(true);
        return;
    }
    set_status(&ui, &s, &format!("Reconnecting… (attempt {n})"));
    glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        if s.generation.get() == generation {
            start_session(ui, s);
        }
    });
}

/// Pull decoded frames on the GTK main loop, latest-wins. The shown session
/// paints them; every session keeps its latest for when its tab is shown.
fn poll_frames(ui: Rc<App>, s: Rc<Session>, rx: Receiver<Picture>, generation: u64) {
    glib::timeout_add_local(Duration::from_millis(8), move || {
        if s.generation.get() != generation {
            return glib::ControlFlow::Break;
        }
        let mut latest = None;
        loop {
            match rx.try_recv() {
                Ok(f) => latest = Some(f),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // The worker ended; stop this per-session timer so it does not
                // accumulate across reconnects.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return glib::ControlFlow::Break
                }
            }
        }
        if let Some(p) = latest {
            // Geometry follows the frame that really paints, so the
            // pointer never maps against a picture that is not on
            // screen (a newer config, or a failed import).
            if show_frame(&ui, &s, p.frame, p.view) {
                s.stream_size.set(p.stream);
                s.stream_view.set(p.view);
                s.stream_scale.set((p.scale_milli.max(1) as f32) / 1000.0);
            }
        }
        glib::ControlFlow::Continue
    });
}

/// Show one frame on the video display; `false` when it could not be
/// shown. On macOS the Metal tier draws zero-copy through the GLArea;
/// every other tier goes through a `gdk::Texture`.
fn show_frame(ui: &Rc<App>, s: &Rc<Session>, frame: Frame, view: (u32, u32)) -> bool {
    #[cfg(target_os = "macos")]
    if let Frame::Metal(f) = frame {
        let f = Rc::new(f);
        if is_active(ui, s) {
            ui.gl.set_frame(f.clone(), ui.video.scale_factor(), view);
            ui.tier_stack.set_visible_child_name("gl");
        }
        *s.last_frame.borrow_mut() = Some((CachedFrame::Metal(f), view));
        return true;
    }
    match frame_texture(frame) {
        Ok(texture) => {
            if is_active(ui, s) {
                ui.frame
                    .set_frame(texture.clone(), ui.video.scale_factor(), view);
                #[cfg(target_os = "macos")]
                ui.tier_stack.set_visible_child_name("texture");
            }
            #[cfg(target_os = "macos")]
            let cached = CachedFrame::Texture(texture.clone());
            *s.last_frame.borrow_mut() = Some((cached, view));
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "frame texture import failed");
            false
        }
    }
}

/// Wrap a decoded frame as a GDK texture: a plain memory texture for
/// the CPU tier. The Metal tier never lands here; the GL widget draws it.
fn frame_texture(f: Frame) -> Result<gdk::Texture, glib::Error> {
    match f {
        #[cfg(target_os = "macos")]
        Frame::Metal(_) => {
            // show_frame routes Metal frames to the GLArea before
            // any texture conversion happens.
            unreachable!("Metal frames are drawn by the GL widget")
        }
        Frame::Bgra(f) => {
            let bytes = glib::Bytes::from_owned(f.pixels);
            Ok(gdk::MemoryTexture::new(
                f.width as i32,
                f.height as i32,
                gdk::MemoryFormat::B8g8r8x8,
                &bytes,
                f.width as usize * 4,
            )
            .upcast())
        }
    }
}

fn poll_status(ui: Rc<App>, s: Rc<Session>, rx: Receiver<Status>, generation: u64) {
    glib::timeout_add_local(Duration::from_millis(100), move || {
        if s.generation.get() != generation {
            return glib::ControlFlow::Break;
        }
        while let Ok(status) = rx.try_recv() {
            match status {
                // Pointer-mapping geometry (stream_size/view/scale) is not
                // read from the config: it follows each decoded frame, so a
                // new config never remaps clicks against the old picture.
                Status::Connected {
                    video,
                    view_width,
                    view_height,
                    fps_cap,
                    headless,
                } => {
                    s.headless.set(headless);
                    s.server_view.set((view_width, view_height));
                    s.fps_cap.set(fps_cap);
                    s.resize_requested.set((0, 0));
                    s.retries.set(0);
                    s.gave_up.set(false);
                    if !s.connected.replace(true) {
                        refresh_tabs(&ui);
                    }
                    set_status(&ui, &s, "");
                    // Visible before the first per-second stats arrive.
                    set_stats(&ui, &s, &video);
                    remember_machine(&ui, &s);
                    // A headless session paints the remote cursor into its
                    // frames; hide the local pointer while it is over the
                    // video, or every frame shows two cursors.
                    update_cursor_hidden(&ui);
                    // A fresh server starts at its own default size; a
                    // reconnect must bring it back to the window. The gate
                    // compares against the view, so a server-chosen
                    // reduced-resolution stream never triggers one. A
                    // background session is resized when it is shown.
                    if is_active(&ui, &s) {
                        request_resize(&ui);
                    }
                }
                Status::Stats {
                    fps,
                    mbit,
                    decode_ms,
                    video,
                    audio_ms,
                } => {
                    let (sw, sh) = s.stream_size.get();
                    let (vw, vh) = s.stream_view.get();
                    let stream = if (sw, sh) == (vw, vh) {
                        format!("{sw}x{sh}")
                    } else {
                        format!("{sw}x{sh} → {vw}x{vh}")
                    };
                    let cap = s.fps_cap.get();
                    set_stats(
                        &ui,
                        &s,
                        &format!(
                            "{video}  {fps:.0} fps  {mbit:.1} Mbit/s  decode {decode_ms:.1} ms  {stream} @{cap}{}",
                            audio_ms.map(|ms| format!("  audio {ms:.0} ms")).unwrap_or_default()
                        ),
                    );
                }
                Status::Cursor {
                    width,
                    height,
                    hot_x,
                    hot_y,
                    argb,
                } => set_remote_cursor(
                    &ui,
                    &s,
                    CursorImage {
                        width,
                        height,
                        hot_x,
                        hot_y,
                        argb: &argb,
                    },
                ),
                // Only the shown machine may take over the local clipboard.
                Status::ClipboardOffer {
                    serial,
                    mime_types,
                    files,
                } => {
                    if is_active(&ui, &s) {
                        if let Some(tx) = s.input_tx.borrow().as_ref() {
                            clipboard_ui::set_remote_offer(tx, serial, mime_types, files);
                        }
                    }
                }
                Status::ClipboardRead { mime_type, reply } => {
                    clipboard_ui::read_local(mime_type, reply)
                }
                Status::ClipboardTransfer { id, progress } => {
                    let cancel_via = s.clone();
                    ui.transfers.update(id, &progress, move |id| {
                        if let Some(tx) = cancel_via.input_tx.borrow().clone() {
                            let _ = tx.send(clipboard::ToWorker::CancelTransfer(id));
                        }
                    });
                }
                Status::Error(e) => {
                    tracing::error!(machine = %s.name, error = %e, "connection failed");
                    set_status(&ui, &s, &format!("Error: {e}"));
                    show_disconnected(&ui, &s);
                    schedule_reconnect(ui.clone(), s.clone(), generation);
                    return glib::ControlFlow::Break;
                }
                Status::Incompatible(message) => {
                    tracing::error!(machine = %s.name, error = %message, "incompatible server");
                    set_status(&ui, &s, &message);
                    show_disconnected(&ui, &s);
                    s.gave_up.set(true);
                    return glib::ControlFlow::Break;
                }
                Status::Closed => {
                    show_disconnected(&ui, &s);
                    schedule_reconnect(ui.clone(), s.clone(), generation);
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

/// Show the remote cursor as the video widget's own cursor, so the local
/// compositor draws it at the real pointer position with no added latency.
/// An image with no visible shape (fully transparent, or one flat colour as
/// Hyprland sends when it has no cursor image to share) falls back to the
/// default pointer so the user is never left without one. Headless sessions
/// ignore shapes: their frames carry the painted-in cursor and the local
/// pointer is hidden while it is over the video (see
/// [`update_cursor_hidden`]), so a widget cursor could only ever be a
/// second pointer.
fn set_remote_cursor(ui: &App, s: &Rc<Session>, image: CursorImage) {
    if s.headless.get() {
        return;
    }
    let cursor = remote_cursor(image);
    if is_active(ui, s) {
        ui.video.set_cursor(cursor.as_ref());
    }
    *s.cursor.borrow_mut() = cursor;
}

/// Hide the local pointer while a headless session's frames already show
/// the remote cursor, and show it again everywhere else. Hiding goes
/// through `NSCursor` rather than a transparent widget cursor because
/// macOS never applies the transparent one (and resets widget cursors to
/// the arrow while a button is held); the painted-in pointer is then the
/// only one on screen. Balanced through `cursor_hidden`, so a session or
/// pointer-state change that resolves to the same state costs nothing.
#[cfg(target_os = "macos")]
fn update_cursor_hidden(ui: &App) {
    let hide = ui.pointer_over_video.get() && active(ui).is_some_and(|s| s.headless.get());
    if hide == ui.cursor_hidden.get() {
        return;
    }
    ui.cursor_hidden.set(hide);
    tracing::debug!(hide, "cursor hiding transition");
    if hide {
        cursor::hide();
    } else {
        cursor::unhide();
    }
}

/// No headless sessions off the Mac; nothing to hide.
#[cfg(not(target_os = "macos"))]
fn update_cursor_hidden(_ui: &App) {}

struct CursorImage<'a> {
    width: u32,
    height: u32,
    hot_x: i32,
    hot_y: i32,
    argb: &'a [u8],
}

fn remote_cursor(image: CursorImage) -> Option<gdk::Cursor> {
    let CursorImage {
        width,
        height,
        hot_x,
        hot_y,
        argb,
    } = image;
    let needed = width as u64 * height as u64 * 4;
    if width == 0 || height == 0 || width > 1024 || height > 1024 || (argb.len() as u64) < needed {
        return None;
    }
    if !has_visible_shape(argb) {
        return None;
    }
    let bytes = glib::Bytes::from(argb);
    let texture = gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        gdk::MemoryFormat::B8g8r8a8,
        &bytes,
        width as usize * 4,
    );
    Some(gdk::Cursor::from_texture(&texture, hot_x, hot_y, None))
}

fn has_visible_shape(argb: &[u8]) -> bool {
    let (pixels, _) = argb.as_chunks::<4>();
    let Some(first) = pixels.first() else {
        return false;
    };
    let opaque = pixels.iter().any(|p| p[3] != 0);
    let uniform = pixels.iter().all(|p| p == first);
    opaque && !uniform
}

/// Map a widget-space point to the remote output's logical coordinates:
/// invert the frame layout to view pixels, scale to physical stream pixels
/// (a reduced-resolution stream is stretched to the view box), then divide
/// by the remote scale, which is what the virtual pointer expects.
fn to_remote(ui: &App, x: f64, y: f64) -> (f64, f64) {
    let Some(s) = active(ui) else {
        return (0.0, 0.0);
    };
    let (rw, rh) = s.stream_size.get();
    let (vw, vh) = s.stream_view.get();
    if rw == 0 || rh == 0 {
        return (0.0, 0.0);
    }
    let device = ui.video.scale_factor().max(1);
    let (aw, ah) = (ui.video.width() as f64, ui.video.height() as f64);
    let Some(l) = paintable::layout((vw, vh), device, aw, ah) else {
        return (0.0, 0.0);
    };
    let vx = (x - l.x) / l.factor * device as f64;
    let vy = (y - l.y) / l.factor * device as f64;
    let px = (vx * rw as f64 / vw as f64).clamp(0.0, rw as f64);
    let py = (vy * rh as f64 / vh as f64).clamp(0.0, rh as f64);
    let scale = s.stream_scale.get().max(0.01) as f64;
    (px / scale, py / scale)
}

fn send(ui: &App, msg: ClientMsg) {
    if let Some(s) = active(ui) {
        send_to(&s, msg);
    }
}

fn send_to(s: &Session, msg: ClientMsg) {
    if let Some(tx) = s.input_tx.borrow().as_ref() {
        let _ = tx.send(clipboard::ToWorker::Send(msg));
    }
}

/// A user-configured hotkey: `none`, a chord, or a double-tap.
#[derive(Clone)]
enum Hotkey {
    None,
    DoubleTap {
        keyval: gdk::Key,
        within: Duration,
    },
    Chord {
        mods: gdk::ModifierType,
        keyval: gdk::Key,
    },
}

const CHORD_MODS: gdk::ModifierType = gdk::ModifierType::CONTROL_MASK
    .union(gdk::ModifierType::ALT_MASK)
    .union(gdk::ModifierType::SHIFT_MASK)
    .union(gdk::ModifierType::SUPER_MASK);

impl Hotkey {
    fn parse(spec: &str) -> Result<Self, String> {
        let s = spec.trim();
        if s.is_empty() || s.eq_ignore_ascii_case("none") {
            return Ok(Self::None);
        }
        let lower = s.to_ascii_lowercase();
        if let Some(rest) = lower
            .strip_prefix("double-")
            .or_else(|| lower.strip_prefix("double:"))
        {
            return Ok(Self::DoubleTap {
                keyval: key_from_name(rest)?,
                within: Duration::from_millis(400),
            });
        }
        let mut mods = gdk::ModifierType::empty();
        let mut keyval = None;
        for tok in s.split('+') {
            match tok.trim().to_ascii_lowercase().as_str() {
                "" => {}
                "ctrl" | "control" => mods |= gdk::ModifierType::CONTROL_MASK,
                "alt" => mods |= gdk::ModifierType::ALT_MASK,
                "shift" => mods |= gdk::ModifierType::SHIFT_MASK,
                "super" | "logo" | "win" | "meta" => mods |= gdk::ModifierType::SUPER_MASK,
                other => keyval = Some(key_from_name(other)?),
            }
        }
        match keyval {
            Some(keyval) => Ok(Self::Chord { mods, keyval }),
            None => Err(format!("no key in hotkey '{spec}'")),
        }
    }

    /// True if this press is the release trigger. For a double-tap it records
    /// the tap time and returns true only on the quick second press, so the
    /// first tap still reaches the remote.
    fn matches(
        &self,
        keyval: gdk::Key,
        state: gdk::ModifierType,
        last_tap: &RefCell<Option<Instant>>,
    ) -> bool {
        match self {
            Self::None => false,
            Self::Chord { mods, keyval: k } => keyval == *k && (state & CHORD_MODS) == *mods,
            Self::DoubleTap { keyval: k, within } => keyval == *k && double_tap(*within, last_tap),
        }
    }

    /// True if this press is the trigger, matched on the physical key.
    /// macOS mutates the keyval under Option — ⌥Space arrives as U+00A0,
    /// ⌥P as π — so a chord carrying ALT can never match its key's name;
    /// the evdev code of the hardware keycode is modifier-stable.
    #[cfg(target_os = "macos")]
    fn matches_physical(
        &self,
        keycode: u16,
        state: gdk::ModifierType,
        last_tap: &RefCell<Option<Instant>>,
    ) -> bool {
        let Some(code) = kbd::evdev_code(keycode) else {
            return false;
        };
        match self {
            Self::None => false,
            Self::Chord { mods, keyval: k } => {
                (state & CHORD_MODS) == *mods && keyval_to_evdev(*k) == Some(code)
            }
            Self::DoubleTap { keyval: k, within } => {
                keyval_to_evdev(*k) == Some(code) && double_tap(*within, last_tap)
            }
        }
    }

    /// The evdev code of the key a match fires on, if the key names one
    /// the table knows.
    #[cfg(target_os = "macos")]
    fn evdev(&self) -> Option<u32> {
        self.keyval().and_then(keyval_to_evdev)
    }

    /// The key a match fires on, if any.
    fn keyval(&self) -> Option<gdk::Key> {
        match self {
            Self::None => None,
            Self::DoubleTap { keyval, .. } | Self::Chord { keyval, .. } => Some(*keyval),
        }
    }

    fn describe(&self) -> String {
        let name = |k: &gdk::Key| {
            let n = k
                .name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".into());
            if n == "Escape" {
                "Esc".into()
            } else {
                n
            }
        };
        match self {
            Self::None => "release disabled".into(),
            Self::DoubleTap { keyval, .. } => format!("double-tap {}", name(keyval)),
            Self::Chord { mods, keyval } => {
                let mut parts = Vec::new();
                if mods.contains(gdk::ModifierType::CONTROL_MASK) {
                    parts.push("Ctrl".to_string());
                }
                if mods.contains(gdk::ModifierType::ALT_MASK) {
                    parts.push("Alt".to_string());
                }
                if mods.contains(gdk::ModifierType::SHIFT_MASK) {
                    parts.push("Shift".to_string());
                }
                if mods.contains(gdk::ModifierType::SUPER_MASK) {
                    parts.push("Super".to_string());
                }
                parts.push(name(keyval));
                parts.join("+")
            }
        }
    }
}

/// Record a tap and report whether it was the quick second one.
fn double_tap(within: Duration, last_tap: &RefCell<Option<Instant>>) -> bool {
    let now = Instant::now();
    let mut lt = last_tap.borrow_mut();
    match *lt {
        Some(prev) if now.duration_since(prev) <= within => {
            *lt = None;
            true
        }
        _ => {
            *lt = Some(now);
            false
        }
    }
}

/// The evdev code a chord key names, through the ANSI virtual keys. The
/// letters and digits come from `to_unicode`; the named specials that
/// carry none are matched on their `gdk::Key` constants.
#[cfg(target_os = "macos")]
fn keyval_to_evdev(key: gdk::Key) -> Option<u32> {
    let kvk: u16 = if key == gdk::Key::Escape {
        0x35
    } else if key == gdk::Key::Return || key == gdk::Key::KP_Enter {
        0x24
    } else if key == gdk::Key::Tab {
        0x30
    } else if key == gdk::Key::space {
        0x31
    } else {
        match key.to_unicode()? {
            'a' => 0x00,
            's' => 0x01,
            'd' => 0x02,
            'f' => 0x03,
            'h' => 0x04,
            'g' => 0x05,
            'z' => 0x06,
            'x' => 0x07,
            'c' => 0x08,
            'v' => 0x09,
            'b' => 0x0B,
            'q' => 0x0C,
            'w' => 0x0D,
            'e' => 0x0E,
            'r' => 0x0F,
            'y' => 0x10,
            't' => 0x11,
            '1' => 0x12,
            '2' => 0x13,
            '3' => 0x14,
            '4' => 0x15,
            '6' => 0x16,
            '5' => 0x17,
            '=' => 0x18,
            '9' => 0x19,
            '7' => 0x1A,
            '-' => 0x1B,
            '8' => 0x1C,
            '0' => 0x1D,
            ']' => 0x1E,
            'o' => 0x1F,
            'u' => 0x20,
            '[' => 0x21,
            'i' => 0x22,
            'p' => 0x23,
            'l' => 0x25,
            'j' => 0x26,
            '\'' => 0x27,
            'k' => 0x28,
            ';' => 0x29,
            '\\' => 0x2A,
            ',' => 0x2B,
            '/' => 0x2C,
            'n' => 0x2D,
            'm' => 0x2E,
            '.' => 0x2F,
            '`' => 0x32,
            _ => return None,
        }
    };
    kbd::evdev_code(kvk)
}

/// Resolve a key name to a `gdk::Key`, accepting lower-case and a few aliases.
fn key_from_name(name: &str) -> Result<gdk::Key, String> {
    let n = name.trim();
    let alias = match n.to_ascii_lowercase().as_str() {
        "esc" => Some("Escape"),
        "enter" | "return" => Some("Return"),
        "space" => Some("space"),
        _ => None,
    };
    let title = {
        let mut c = n.chars();
        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
    };
    for cand in alias.iter().copied().chain([n, title.as_str()]) {
        if !cand.is_empty() {
            if let Some(k) = gdk::Key::from_name(cand) {
                return Ok(k);
            }
        }
    }
    Err(format!("unknown key '{name}'"))
}

/// Give the keyboard back to the local compositor by dropping video focus,
/// which fires the focus-leave handler that restores system shortcuts.
/// True while the address bar shows and holds the focus. GTK4 focuses the
/// text inside the entry, so ask the window for the focus widget and walk
/// up; a folded address bar is unmapped.
fn address_bar_in_use(window: &adw::ApplicationWindow, entry: &gtk::Entry) -> bool {
    entry.is_mapped()
        && gtk::prelude::GtkWindowExt::focus(window)
            .is_some_and(|f| f == *entry || f.is_ancestor(entry))
}

fn release_capture(ui: &Rc<App>, window: &adw::ApplicationWindow) {
    ui.released.set(true);
    gtk::prelude::GtkWindowExt::set_focus(window, gtk::Widget::NONE);
    show_hint(ui, "Shortcuts released — click the screen to capture again");
}

/// Release every key held on the remote. Once the video loses focus their
/// local release events never reach us, so the remote would keep (say) Shift
/// down until the next connection.
fn release_pressed_keys(ui: &App) {
    let held = std::mem::take(&mut *ui.pressed_keys.borrow_mut());
    for code in held {
        send(
            ui,
            ClientMsg::Key {
                keycode: code,
                pressed: false,
            },
        );
    }
}

/// Record a local key event. Returns false when it should not be forwarded:
/// an auto-repeat press (the remote compositor repeats on its own) or a
/// release of a key whose press was never sent.
fn track_key(ui: &App, code: u32, pressed: bool) -> bool {
    let mut keys = ui.pressed_keys.borrow_mut();
    if pressed {
        keys.insert(code)
    } else {
        keys.remove(&code)
    }
}

// Linux evdev keycodes — the codes `kbd` maps every platform's keys to,
// named here because the launcher chord synthesizes keys with no local
// event to translate.
const KEY_LEFTCTRL: u32 = 29;
const KEY_LEFTALT: u32 = 56;
const KEY_SPACE: u32 = 57;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_LEFTMETA: u32 = 125;

/// The evdev codes of a chord's modifier keys, in a fixed order.
fn chord_mod_codes(mods: gdk::ModifierType) -> Vec<u32> {
    let mut codes = Vec::new();
    if mods.contains(gdk::ModifierType::CONTROL_MASK) {
        codes.push(KEY_LEFTCTRL);
    }
    if mods.contains(gdk::ModifierType::ALT_MASK) {
        codes.push(KEY_LEFTALT);
    }
    if mods.contains(gdk::ModifierType::SHIFT_MASK) {
        codes.push(KEY_LEFTSHIFT);
    }
    if mods.contains(gdk::ModifierType::SUPER_MASK) {
        codes.push(KEY_LEFTMETA);
    }
    codes
}

/// Record and forward a key event, skipping auto-repeats and releases of
/// keys whose press never went.
fn forward_key(ui: &App, code: u32, pressed: bool) {
    if track_key(ui, code, pressed) {
        send(
            ui,
            ClientMsg::Key {
                keycode: code,
                pressed,
            },
        );
    }
}

/// Begin the launcher chord on the remote: lift the modifiers the chord
/// was struck with — their key events already went out, so the remote
/// holds them — then press super and space there instead.
fn arm_launcher(ui: &App, launcher: &Hotkey) {
    if let Hotkey::Chord { mods, .. } = launcher {
        for code in chord_mod_codes(*mods) {
            forward_key(ui, code, false);
        }
    }
    tracing::debug!("launcher chord armed: super+space down on the remote");
    forward_key(ui, KEY_LEFTMETA, true);
    forward_key(ui, KEY_SPACE, true);
}

/// End the launcher chord: lift space and super, then press back the
/// chord modifiers still held locally, so the remote mirrors the
/// physical keyboard again.
fn finish_launcher(ui: &App, launcher: &Hotkey, armed: &Cell<bool>, state: gdk::ModifierType) {
    tracing::debug!("launcher chord finished: super+space up on the remote");
    forward_key(ui, KEY_SPACE, false);
    forward_key(ui, KEY_LEFTMETA, false);
    if let Hotkey::Chord { mods, .. } = launcher {
        for code in chord_mod_codes(state & *mods) {
            forward_key(ui, code, true);
        }
    }
    armed.set(false);
}

/// The macOS menu shortcuts we handle locally. Matched on the `kVK_*`
/// keycode, so the physical key works on any keyboard layout.
#[cfg(target_os = "macos")]
enum LocalShortcut {
    /// ⌘Q: close the window, ending the app and with it every session.
    Quit,
    /// ⌘W: close the shown machine's tab — the address bar's edit first,
    /// a running session next, the window when there is neither.
    CloseTab,
    /// ⌘T/⌘N: the app is one tabbed window, so both open the address bar
    /// for a new connection.
    NewTab,
}

/// The local ⌘ shortcut for a key event, if any. The macOS backend reports
/// Command as `META_MASK` (`SUPER_MASK` is accepted too, in case a future
/// GTK maps it there). Chords carrying other modifiers — ⌘⇧Q, ⌘⌥W, ... —
/// are not ours and fall through.
#[cfg(target_os = "macos")]
fn local_shortcut(keycode: u16, state: gdk::ModifierType) -> Option<LocalShortcut> {
    use gdk::ModifierType as M;
    if !state.intersects(M::META_MASK | M::SUPER_MASK)
        || state.intersects(M::CONTROL_MASK | M::ALT_MASK | M::SHIFT_MASK)
    {
        return None;
    }
    Some(match keycode {
        0x0C => LocalShortcut::Quit,          // kVK_ANSI_Q
        0x0D => LocalShortcut::CloseTab,      // kVK_ANSI_W
        0x11 | 0x2D => LocalShortcut::NewTab, // kVK_ANSI_T, kVK_ANSI_N
        _ => return None,
    })
}

/// The macOS window shortcuts — ⌘Q, ⌘W, ⌘T, ⌘N — on the window itself,
/// in the bubble phase.
///
/// That ordering is the whole trick: while the keyboard is captured the
/// picture's own key controller consumes every event first and returns
/// `Stop`, so a captured ⌘W closes the remote's window, not our tab. This
/// controller only ever sees keys nobody local claimed: with the capture
/// released, before the picture is focused, or while the address bar is
/// in use.
#[cfg(target_os = "macos")]
fn install_local_shortcuts(ui: &Rc<App>) {
    let key = gtk::EventControllerKey::new();
    {
        let ui = ui.clone();
        key.connect_key_pressed(move |_, _keyval, keycode, state| {
            let Some(shortcut) = local_shortcut(keycode as u16, state) else {
                return glib::Propagation::Proceed;
            };
            match shortcut {
                LocalShortcut::Quit => {
                    ui.window.close();
                }
                LocalShortcut::CloseTab => {
                    if address_bar_in_use(&ui.window, &ui.entry) {
                        cancel_address_edit(&ui);
                    } else if let Some(s) = active(&ui) {
                        stop_session(&ui, &s.name);
                    } else {
                        ui.window.close();
                    }
                }
                LocalShortcut::NewTab => {
                    show_address_bar(&ui, true);
                    ui.entry.grab_focus();
                }
            }
            glib::Propagation::Stop
        });
    }
    ui.window.add_controller(key);
}

fn install_input_handlers(
    ui: &Rc<App>,
    video: &gtk::Widget,
    window: &adw::ApplicationWindow,
    hotkey: Hotkey,
    launcher: Hotkey,
) {
    video.set_focusable(true);
    video.set_can_focus(true);

    // Keyboard: the evdev code is the hardware keycode minus 8 on
    // Linux, and the positional kVK table on macOS. The controller
    // runs in the capture phase so that while the picture has the
    // keyboard every key goes to the remote, ahead of any local
    // handling.
    //
    // The launcher chord is the one translation on that path: it holds
    // super+space on the remote for as long as the chord is held here.
    let key = gtk::EventControllerKey::new();
    let last_tap: Rc<RefCell<Option<Instant>>> = Rc::new(RefCell::new(None));
    let launcher_tap: Rc<RefCell<Option<Instant>>> = Rc::new(RefCell::new(None));
    let launcher_armed = Rc::new(Cell::new(false));
    {
        let ui = ui.clone();
        let window = window.clone();
        let hotkey = hotkey.clone();
        let launcher = launcher.clone();
        let last_tap = last_tap.clone();
        let launcher_tap = launcher_tap.clone();
        let launcher_armed = launcher_armed.clone();
        key.connect_key_pressed(move |_, keyval, keycode, state| {
            if hotkey.matches(keyval, state, &last_tap) {
                release_pressed_keys(&ui);
                release_capture(&ui, &window);
                return glib::Propagation::Stop;
            }
            // The launcher chord translates to super+space on the remote.
            // Any other key while it is held completes it first, so its
            // super never leaks into an ordinary chord.
            #[cfg(target_os = "macos")]
            let chord = launcher.matches_physical(keycode as u16, state, &launcher_tap);
            if launcher_armed.get() && !chord {
                finish_launcher(&ui, &launcher, &launcher_armed, state);
            }
            if chord {
                if !launcher_armed.replace(true) {
                    arm_launcher(&ui, &launcher);
                }
                return glib::Propagation::Stop;
            }
            // GDK on macOS reports kVK_* virtual keys, translated
            // positionally to evdev codes.
            #[cfg(target_os = "macos")]
            let Some(code) = kbd::evdev_code(keycode as u16) else {
                return glib::Propagation::Stop;
            };
            tracing::debug!(keycode, code, "key pressed");
            forward_key(&ui, code, true);
            glib::Propagation::Stop
        });
    }
    {
        let ui = ui.clone();
        let launcher = launcher.clone();
        let launcher_armed = launcher_armed.clone();
        key.connect_key_released(move |_, _keyval, keycode, state| {
            #[cfg(target_os = "macos")]
            let chord_key = launcher.evdev() == kbd::evdev_code(keycode as u16);
            if launcher_armed.get() && chord_key {
                finish_launcher(&ui, &launcher, &launcher_armed, state);
                return;
            }
            #[cfg(target_os = "macos")]
            let Some(code) = kbd::evdev_code(keycode as u16) else {
                return;
            };
            tracing::debug!(keycode, code, "key released");
            forward_key(&ui, code, false);
        });
    }
    video.add_controller(key);

    // Pointer motion, and the capture itself: the remote gets the keyboard
    // only while the pointer is over the picture, so the header bar and
    // anything else outside it stay local.
    let motion = gtk::EventControllerMotion::new();
    {
        let ui = ui.clone();
        motion.connect_motion(move |_, x, y| {
            let (rx, ry) = to_remote(&ui, x, y);
            send(&ui, ClientMsg::PointerMotion { x: rx, y: ry });
        });
    }
    {
        let ui = ui.clone();
        let video = video.clone();
        motion.connect_enter(move |_, _, _| {
            ui.pointer_over_video.set(true);
            update_cursor_hidden(&ui);
            if active(&ui).is_some()
                && !ui.released.get()
                && !address_bar_in_use(&ui.window, &ui.entry)
            {
                video.grab_focus();
            }
        });
    }
    {
        let ui = ui.clone();
        let window = window.clone();
        motion.connect_leave(move |_| {
            ui.pointer_over_video.set(false);
            update_cursor_hidden(&ui);
            // A deliberate release lasts only as long as the pointer rests on
            // the picture; leaving and coming back captures again.
            ui.released.set(false);
            if gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|f| f == ui.video) {
                gtk::prelude::GtkWindowExt::set_focus(&window, gtk::Widget::NONE);
            }
        });
    }
    video.add_controller(motion);

    // Buttons.
    let click = gtk::GestureClick::new();
    click.set_button(0); // any button
    {
        let ui = ui.clone();
        let video = video.clone();
        click.connect_pressed(move |g, _, _, _| {
            ui.released.set(false);
            video.grab_focus();
            send(
                &ui,
                ClientMsg::PointerButton {
                    button: evdev_button(g.current_button()),
                    pressed: true,
                },
            );
        });
    }
    {
        let ui = ui.clone();
        click.connect_released(move |g, _, _, _| {
            send(
                &ui,
                ClientMsg::PointerButton {
                    button: evdev_button(g.current_button()),
                    pressed: false,
                },
            );
        });
    }
    video.add_controller(click);

    // Scroll.
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    {
        let ui = ui.clone();
        scroll.connect_scroll(move |_, dx, dy| {
            if dy != 0.0 {
                send(
                    &ui,
                    ClientMsg::PointerAxis {
                        axis: Axis::Vertical,
                        value: dy * 15.0,
                        discrete: Some(dy.signum() as i32),
                        stop: false,
                    },
                );
            }
            if dx != 0.0 {
                send(
                    &ui,
                    ClientMsg::PointerAxis {
                        axis: Axis::Horizontal,
                        value: dx * 15.0,
                        discrete: Some(dx.signum() as i32),
                        stop: false,
                    },
                );
            }
            glib::Propagation::Stop
        });
    }
    video.add_controller(scroll);

    // While the video has focus, route system shortcuts (Super, Alt-Tab, ...)
    // to the remote session instead of the local compositor.
    let focus = gtk::EventControllerFocus::new();
    {
        let window = window.clone();
        let ui = ui.clone();
        let mut parts = Vec::new();
        if !matches!(hotkey, Hotkey::None) {
            parts.push(format!("{} to release", hotkey.describe()));
        }
        if !matches!(launcher, Hotkey::None) {
            parts.push(format!("{} opens the remote launcher", launcher.describe()));
        }
        let hint =
            (!parts.is_empty()).then(|| format!("Shortcuts captured — {}", parts.join(", ")));
        focus.connect_enter(move |_| {
            tracing::debug!("video focused; inhibiting system shortcuts");
            if let Some(toplevel) = window.surface().and_downcast::<gdk::Toplevel>() {
                toplevel.inhibit_system_shortcuts(None::<&gdk::Event>);
                // The compositor answers asynchronously, and may refuse.
                toplevel.connect_shortcuts_inhibited_notify(|t| {
                    tracing::info!(
                        inhibited = t.is_shortcuts_inhibited(),
                        "compositor shortcut inhibit"
                    );
                });
            }
            if let Some(hint) = &hint {
                if !ui.capture_hinted.replace(true) {
                    show_hint(&ui, hint);
                }
            }
        });
    }
    {
        let window = window.clone();
        let ui = ui.clone();
        focus.connect_leave(move |_| {
            tracing::debug!("video unfocused; restoring system shortcuts");
            release_pressed_keys(&ui);
            if let Some(toplevel) = window.surface().and_downcast::<gdk::Toplevel>() {
                toplevel.restore_system_shortcuts();
            }
        });
    }
    video.add_controller(focus);
}

/// In fullscreen the header leaves the layout, so the picture gets the whole
/// screen, and slides in over it while the pointer is at the top edge. The header stays while the address bar is in use, so
/// it does not slide away under a half-typed machine.
fn install_fullscreen_bars(
    window: &adw::ApplicationWindow,
    content: &gtk::Box,
    overlay: &gtk::Overlay,
    header: &adw::HeaderBar,
    fullscreen_btn: &gtk::ToggleButton,
    entry: &gtk::Entry,
) {
    let address_bar_in_use = {
        let (window, entry) = (window.clone(), entry.clone());
        Rc::new(move || address_bar_in_use(&window, &entry))
    };
    let top = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .valign(gtk::Align::Start)
        .build();
    overlay.add_overlay(&top);

    {
        let (content, header) = (content.clone(), header.clone());
        let (top, fullscreen_btn) = (top.clone(), fullscreen_btn.clone());
        window.connect_fullscreened_notify(move |w| {
            let full = w.is_fullscreen();
            if fullscreen_btn.is_active() != full {
                fullscreen_btn.set_active(full);
            }
            if full {
                content.remove(&header);
                top.set_child(Some(&header));
            } else {
                top.set_reveal_child(false);
                top.set_child(None::<&gtk::Widget>);
                content.prepend(&header);
            }
        });
    }

    const EDGE: f64 = 2.0;
    const LEAVE_MARGIN: f64 = 8.0;
    let motion = gtk::EventControllerMotion::new();
    {
        let (window, header, top) = (window.clone(), header.clone(), top.clone());
        let address_bar_in_use = address_bar_in_use.clone();
        motion.connect_motion(move |_, _, y| {
            if !window.is_fullscreen() {
                return;
            }
            if y <= EDGE {
                top.set_reveal_child(true);
            } else if top.reveals_child()
                && y > header.height() as f64 + LEAVE_MARGIN
                && !address_bar_in_use()
            {
                top.set_reveal_child(false);
            }
        });
    }
    {
        let top = top.clone();
        let address_bar_in_use = address_bar_in_use.clone();
        motion.connect_leave(move |_| {
            if !address_bar_in_use() {
                top.set_reveal_child(false);
            }
        });
    }
    overlay.add_controller(motion);

    // Keyboard dismissal (Escape, Enter) moves focus without a pointer
    // event; hide the header then, unless the pointer still rests on it.
    {
        let (window, top, header) = (window.clone(), top.clone(), header.clone());
        window.clone().connect_focus_widget_notify(move |_| {
            if !window.is_fullscreen() || address_bar_in_use() {
                return;
            }
            let pointer_on_header = WidgetExt::display(&window)
                .default_seat()
                .and_then(|s| s.pointer())
                .and_then(|p| window.surface().and_then(|s| s.device_position(&p)))
                .is_some_and(|(_, y, _)| y <= header.height() as f64 + LEAVE_MARGIN);
            if !pointer_on_header {
                top.set_reveal_child(false);
            }
        });
    }
}

/// Ask the server to match the window, once the size has settled for 200 ms
/// so a drag-resize does not restart the encoder on every step. The
/// decoder's output zoom is requested on each poll that changes it, with
/// no settling delay: on the GPU tier the change costs only a reallocation
/// of the display images and a redraw, and the CPU tier picks it up with
/// its next frame.
fn install_resize_handler(ui: &Rc<App>) {
    // A Picture has no resize signal, so poll its allocation; the one-shot
    // timer sends only once the size has held for 200 ms.
    let ui = ui.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let scale = ui.video.scale_factor();
        ui.frame.set_scale(scale);
        #[cfg(target_os = "macos")]
        ui.gl.set_scale(scale);
        request_zoom(&ui);
        let (w, h) = (ui.video.width() * scale, ui.video.height() * scale);
        let size = (w.max(0) as u32 & !1, h.max(0) as u32 & !1);
        if size != ui.view_size.get() {
            ui.view_size.set(size);
            let ui = ui.clone();
            glib::timeout_add_local_once(Duration::from_millis(200), move || {
                if ui.view_size.get() == size {
                    request_resize(&ui);
                }
            });
        }
        glib::ControlFlow::Continue
    });
}

/// Ask the decoder for the integer zoom the current window allows, when it
/// differs from the last request. The view comes from the latest
/// StreamConfig, falling back to the last painted frame.
fn request_zoom(ui: &App) {
    let Some(s) = active(ui) else {
        return;
    };
    let mut view = s.server_view.get();
    if view == (0, 0) {
        view = s.stream_view.get();
    }
    let device = ui.video.scale_factor().max(1);
    let (aw, ah) = (ui.video.width() as f64, ui.video.height() as f64);
    let want = paintable::layout(view, device, aw, ah).map_or(1, |l| l.zoom());
    if want == s.zoom.get() {
        return;
    }
    s.zoom.set(want);
    let tx = s.input_tx.borrow().clone();
    if let Some(tx) = tx {
        let _ = tx.send(clipboard::ToWorker::Zoom(want));
    }
}

/// Send a Resize if the server's view does not already match the window.
/// The gate compares the server's VIEW, not the stream: a stream the server
/// chose to send at reduced resolution is not a size mismatch, and asking
/// again would fight the server's own choice.
fn request_resize(ui: &App) {
    let Some(s) = active(ui) else {
        return;
    };
    let size = ui.view_size.get();
    if size.0 < 64 || size.1 < 64 || size == s.server_view.get() || size == s.resize_requested.get()
    {
        return;
    }
    s.resize_requested.set(size);
    send_to(
        &s,
        ClientMsg::Resize {
            width: size.0,
            height: size.1,
            scale: ui.video.scale_factor() as f32,
        },
    );
}

/// GTK button number to evdev `BTN_*`.
fn evdev_button(n: u32) -> u32 {
    match n {
        1 => 0x110, // BTN_LEFT
        2 => 0x112, // BTN_MIDDLE
        3 => 0x111, // BTN_RIGHT
        8 => 0x116, // BTN_SIDE (back)
        9 => 0x115, // BTN_EXTRA (forward)
        _ => 0x110,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn visible_shape_needs_alpha_and_contrast() {
        let transparent = [0u8; 16];
        let black = [0, 0, 0, 255].repeat(4);
        let mut arrow = [0u8; 16];
        arrow[3] = 255;
        assert!(!has_visible_shape(&transparent));
        assert!(!has_visible_shape(&black));
        assert!(has_visible_shape(&arrow));
    }

    #[test]
    fn endpoints_carry_the_mode_and_report_back() {
        let cli = Cli::parse_from(["mac-gliff"]);
        let mirror = ssh_endpoint_with_mode(&cli, "user@host", DisplayMode::Mirror);
        let window = ssh_endpoint_with_mode(&cli, "user@host", DisplayMode::Window);
        let Endpoint::Ssh(m) = &mirror else {
            panic!("not an ssh endpoint");
        };
        assert_eq!(m.server_args, vec!["--output", "auto"]);
        let Endpoint::Ssh(w) = &window else {
            panic!("not an ssh endpoint");
        };
        assert_eq!(w.server_args, vec!["--headless"]);
        assert_eq!(
            session_mode(&Session::new(mirror)),
            Some(DisplayMode::Mirror)
        );
        assert_eq!(
            session_mode(&Session::new(window)),
            Some(DisplayMode::Window)
        );
        assert_eq!(
            session_mode(&Session::new(Endpoint::Tcp("localhost:1".into()))),
            None
        );
    }

    #[test]
    fn parse_chord_and_double_and_none() {
        assert!(matches!(Hotkey::parse("none").unwrap(), Hotkey::None));
        assert!(matches!(Hotkey::parse("").unwrap(), Hotkey::None));

        match Hotkey::parse("shift+escape").unwrap() {
            Hotkey::Chord { mods, keyval } => {
                assert_eq!(mods, gdk::ModifierType::SHIFT_MASK);
                assert_eq!(keyval, gdk::Key::Escape);
            }
            _ => panic!("expected chord"),
        }

        match Hotkey::parse("Ctrl+Alt+q").unwrap() {
            Hotkey::Chord { mods, .. } => {
                assert!(mods.contains(gdk::ModifierType::CONTROL_MASK));
                assert!(mods.contains(gdk::ModifierType::ALT_MASK));
            }
            _ => panic!("expected chord"),
        }

        assert!(matches!(
            Hotkey::parse("double-escape").unwrap(),
            Hotkey::DoubleTap { .. }
        ));
        assert!(Hotkey::parse("ctrl+alt").is_err());
    }

    #[test]
    fn chord_matches_only_with_exact_mods() {
        let hk = Hotkey::parse("shift+escape").unwrap();
        let lt = RefCell::new(None);
        assert!(hk.matches(gdk::Key::Escape, gdk::ModifierType::SHIFT_MASK, &lt));
        // Bare Escape, no Shift: not a match (so it reaches the remote).
        assert!(!hk.matches(gdk::Key::Escape, gdk::ModifierType::empty(), &lt));
        // Extra lock bits are ignored.
        assert!(hk.matches(
            gdk::Key::Escape,
            gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::LOCK_MASK,
            &lt
        ));
    }

    #[test]
    fn double_tap_needs_two_quick_presses() {
        let hk = Hotkey::parse("double-escape").unwrap();
        let lt = RefCell::new(None);
        let none = gdk::ModifierType::empty();
        assert!(!hk.matches(gdk::Key::Escape, none, &lt)); // first tap forwarded
        assert!(hk.matches(gdk::Key::Escape, none, &lt)); // quick second releases
        assert!(!hk.matches(gdk::Key::Escape, none, &lt)); // counter reset
    }

    /// The default chord after the ⌥Space discovery: ⌃Space, exactly.
    #[cfg(target_os = "macos")]
    #[test]
    fn launcher_chord_default_is_ctrl_space() {
        assert_eq!(default_launcher_hotkey(), "ctrl+space");
    }

    #[test]
    fn launcher_chord_is_exact_alt_space() {
        let hk = Hotkey::parse("alt+space").unwrap();
        let lt = RefCell::new(None);
        let alt = gdk::ModifierType::ALT_MASK;
        let space = key_from_name("space").unwrap();
        assert!(hk.matches(space, alt, &lt));
        assert!(!hk.matches(space, gdk::ModifierType::empty(), &lt));
        assert!(!hk.matches(space, alt | gdk::ModifierType::SHIFT_MASK, &lt));
        assert!(!hk.matches(gdk::Key::Escape, alt, &lt));
        assert_eq!(hk.keyval(), Some(space));
    }

    /// The chord modifiers map to the evdev keys the remote expects, so the
    /// launcher translation lifts and restores exactly what it struck.
    #[test]
    fn chord_mod_codes_follow_their_keys() {
        use gdk::ModifierType as M;
        assert_eq!(chord_mod_codes(M::empty()), Vec::<u32>::new());
        assert_eq!(chord_mod_codes(M::CONTROL_MASK), vec![29]);
        assert_eq!(chord_mod_codes(M::ALT_MASK), vec![56]);
        assert_eq!(chord_mod_codes(M::SHIFT_MASK), vec![42]);
        assert_eq!(chord_mod_codes(M::SUPER_MASK), vec![125]);
        assert_eq!(chord_mod_codes(M::CONTROL_MASK | M::ALT_MASK), vec![29, 56]);
    }

    /// On macOS the launcher chord matches the physical key, not the
    /// keyval: Option mutates keyvals (⌥Space arrives as U+00A0), so the
    /// match rides on the hardware keycode's evdev code.
    #[cfg(target_os = "macos")]
    #[test]
    fn launcher_chord_matches_the_physical_key() {
        use gdk::ModifierType as M;
        let hk = Hotkey::parse("alt+space").unwrap();
        let lt = RefCell::new(None);

        assert!(hk.matches_physical(0x31, M::ALT_MASK, &lt)); // kVK_Space
        assert!(!hk.matches_physical(0x31, M::empty(), &lt));
        assert!(!hk.matches_physical(0x31, M::ALT_MASK | M::SHIFT_MASK, &lt));
        assert!(!hk.matches_physical(0x23, M::ALT_MASK, &lt)); // kVK_ANSI_P
        assert_eq!(hk.evdev(), Some(57));
    }

    /// The keyval table covers the keys a chord is struck with.
    #[cfg(target_os = "macos")]
    #[test]
    fn keyvals_map_through_the_ansi_table() {
        assert_eq!(keyval_to_evdev(gdk::Key::Escape), Some(1));
        assert_eq!(keyval_to_evdev(gdk::Key::Return), Some(28));
        assert_eq!(keyval_to_evdev(gdk::Key::Tab), Some(15));
        assert_eq!(keyval_to_evdev(key_from_name("space").unwrap()), Some(57));
        assert_eq!(keyval_to_evdev(key_from_name("p").unwrap()), Some(25));
        assert_eq!(keyval_to_evdev(key_from_name("a").unwrap()), Some(30));
        assert_eq!(keyval_to_evdev(key_from_name("q").unwrap()), Some(16));
    }

    /// The ⌘ shortcuts fire on the bare Command chord of the right key —
    /// never with extra modifiers, never without Command, and only for the
    /// four keys we own; anything else still reaches whoever wants it.
    #[cfg(target_os = "macos")]
    #[test]
    fn local_shortcut_needs_bare_command_and_own_keys() {
        use gdk::ModifierType as M;
        let cmd = M::META_MASK;

        assert!(matches!(
            local_shortcut(0x0C, cmd), // kVK_ANSI_Q
            Some(LocalShortcut::Quit)
        ));
        assert!(matches!(
            local_shortcut(0x0D, cmd), // kVK_ANSI_W
            Some(LocalShortcut::CloseTab)
        ));
        assert!(matches!(
            local_shortcut(0x11, cmd), // kVK_ANSI_T
            Some(LocalShortcut::NewTab)
        ));
        assert!(matches!(
            local_shortcut(0x2D, cmd), // kVK_ANSI_N
            Some(LocalShortcut::NewTab)
        ));
        // A future GTK mapping Command onto SUPER_MASK keeps working.
        assert!(matches!(
            local_shortcut(0x0C, M::SUPER_MASK),
            Some(LocalShortcut::Quit)
        ));

        // No Command: goes to whoever else wants the key.
        assert!(local_shortcut(0x0C, M::empty()).is_none());
        // Wrong or extra modifiers are not our chord.
        assert!(local_shortcut(0x0C, cmd | M::SHIFT_MASK).is_none());
        assert!(local_shortcut(0x0D, cmd | M::CONTROL_MASK).is_none());
        assert!(local_shortcut(0x11, cmd | M::ALT_MASK).is_none());
        assert!(local_shortcut(0x0C, M::CONTROL_MASK).is_none());
        // ⌘ with any other key is the remote's business.
        assert!(local_shortcut(0x00, cmd).is_none()); // kVK_ANSI_A
                                                      // Command itself is not a shortcut.
        assert!(local_shortcut(0x37, cmd).is_none()); // kVK_Command
    }
}
