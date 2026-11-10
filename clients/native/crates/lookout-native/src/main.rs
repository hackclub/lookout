//! Lookout — native desktop client (internal codename "Lookout Phoenix").
//!
//! A GPUI rewrite of the Tauri app in `clients/desktop`, driving the same
//! Rust recording engine (`lookout-core`) in-process.

// Release builds are Windows GUI apps: without this, Windows opens a console
// window beside Lookout, and every `lookout://` link (a second launch that
// hands the link over and exits) flashes one too. Debug builds keep the
// console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

// The design-token modules and component builders mirror the web app's full
// surface; not every token or option is used by a screen yet.
#![allow(dead_code)]

mod app;
mod assets;
mod crash;
mod deeplink;
mod diagnosis;
mod editor_math;
mod filmstrip;
mod fmt;
mod hang;
mod logbuf;
mod previews;
mod programs;
mod images;
mod installer;
mod motion;
mod platform;
mod player;
mod screens;
mod services;
mod smoke;
mod store;
mod theme;
mod ui;
mod updater;
mod windows;

use std::borrow::Cow;

use gpui_kit::gpui::App;

fn main() {
    // WebKitGTK's DMA-BUF renderer aborts the whole app (SIGABRT inside
    // gtk_main_iteration, LOOKOUT-NATIVE-G) when the panel webview lives in
    // an offscreen window on Wayland, at least on AMD/Ubuntu 26.04. That view
    // renders in software anyway. Set before any thread exists.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: first thing in main, still single-threaded.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    logbuf::init();
    // The crash reporter is this binary run again: send it straight into
    // `crash::init` (which never returns for it) before it can touch the
    // single-instance socket, the updater or a window.
    if crash::is_reporter_process() {
        crash::init();
        std::process::exit(0);
    }
    // `lookout.exe` is its own installer and uninstaller (see installer/).
    match installer::mode() {
        installer::Mode::App => {}
        mode => std::process::exit(installer::run(mode)),
    }
    #[cfg(windows)]
    installer::ops::remove_leftovers();
    // Before any window exists: macOS 26 would otherwise freeze every window
    // after its first frame (see platform/macos_occlusion.rs).
    #[cfg(target_os = "macos")]
    platform::macos_occlusion::install();
    platform::reduce_motion::observe();
    if let Ok(f) = std::env::var("LOOKOUT_SLOWMO") {
        if let Ok(f) = f.parse::<f32>() {
            motion::set_slowmo(f);
        }
    }

    // A second launch forwards its deep links to the running app and exits.
    let initial_urls = deeplink::from_args();
    let forwarded = match platform::single_instance::acquire(&initial_urls) {
        platform::single_instance::Instance::Secondary => {
            log::info!("[app] already running — handed over {} link(s)", initial_urls.len());
            return;
        }
        platform::single_instance::Instance::Primary(rx) => rx,
    };
    // Only the primary instance reports (a link hand-off exits at once), and
    // before the updater so a crash while installing one is caught too.
    let _sentry = crash::init();
    crash::self_test();
    // Before any window: roll back a version that never became healthy, or
    // install a staged update and relaunch (exits in both cases).
    updater::boot(&initial_urls);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("lookout-core")
        .enable_all()
        .build()
        .expect("tokio runtime");
    let rt = runtime.handle().clone();

    let app = gpui_kit::platform::application().with_assets(assets::Assets);
    // macOS delivers `lookout://` links as Apple Events.
    let (open_tx, open_rx) = flume::unbounded::<Vec<String>>();
    app.on_open_urls(move |urls| {
        let _ = open_tx.send(urls);
    });
    // macOS keeps running with no window once the main window is closed;
    // launching it again (Dock, Spotlight, Raycast) has to bring one back.
    app.on_reopen(windows::reopen);
    app.run(move |cx: &mut App| {
        cx.text_system()
            .add_fonts(assets::FONTS.iter().map(|b| Cow::Borrowed(*b)).collect())
            .expect("load bundled fonts");
        let families = cx.text_system().all_font_names().into_iter().map(Into::into).collect::<Vec<_>>();
        ui::text::init_family(&families);

        let mut persisted = store::Persisted::load();
        if let Ok(seed) = std::env::var("LOOKOUT_SEED_TOKENS") {
            for t in seed.split(',').map(str::trim).filter(|t| !t.is_empty()).rev() {
                persisted.add_token(t);
            }
        }
        cx.set_global(app::Store(persisted));
        cx.set_global(theme::Theme::dark());
        images::init(cx);
        previews::init(cx);
        cx.set_global(screens::source_picker::RowStates::default());
        ui::input::bind_keys(cx);
        platform::app_menu::init(cx);
        app::init(cx, rt.clone());
        hang::start(cx);
        // First launch: the onboarding window comes first, and the app
        // proper starts once it's finished.
        let launch = Launch { rt: rt.clone(), initial_urls: initial_urls.clone(), forwarded: forwarded.clone(), open_rx: open_rx.clone() };
        if windows::onboarding::wanted(cx) {
            // Links (a `lookout://` launch, or ones clicked meanwhile) wait
            // for the onboarding: each one brings it forward, and they open
            // once it's finished.
            let held: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
            let waiting: std::rc::Rc<std::cell::Cell<bool>> = std::rc::Rc::new(std::cell::Cell::new(true));
            let (fwd, open) = (launch.forwarded.clone(), launch.open_rx.clone());
            for rx in [fwd, open] {
                let (held, waiting) = (held.clone(), waiting.clone());
                cx.spawn(async move |cx| {
                    while waiting.get() {
                        let Ok(urls) = rx.recv_async().await else { break };
                        held.borrow_mut().extend(urls.into_iter().filter(|u| u.starts_with("lookout://")));
                        cx.update(windows::onboarding::bring_forward);
                        if !waiting.get() {
                            // Finished meanwhile: hand them straight on.
                            let urls = std::mem::take(&mut *held.borrow_mut());
                            cx.update(|cx| {
                                windows::focus_main(cx);
                                if let Some(h) = cx.try_global::<windows::MainHandle>().map(|h| h.0) {
                                    h.update(cx, |mw, _, cx| if !urls.is_empty() { mw.handle_deep_links(urls, cx) }).ok();
                                }
                            });
                        }
                    }
                })
                .detach();
            }
            windows::onboarding::open(cx, move |cx| {
                waiting.set(false);
                let mut launch = launch;
                launch.initial_urls.extend(std::mem::take(&mut *held.borrow_mut()));
                start_app(launch, cx)
            });
        } else {
            start_app(launch, cx);
        }
    });
    crash::on_exit();
    drop(runtime);
}

/// What the app proper needs to start (after onboarding, if any).
struct Launch {
    rt: tokio::runtime::Handle,
    initial_urls: Vec<String>,
    forwarded: flume::Receiver<Vec<String>>,
    open_rx: flume::Receiver<Vec<String>>,
}

/// Open the main window and wire the tray, relaunch restore and deep links.
fn start_app(launch: Launch, cx: &mut App) {
    let main = windows::main_window::open(cx);
    smoke::start(cx);
    smoke::start_tour(cx);
    cx.set_global(windows::MainHandle(main));
    windows::wire_tray(launch.rt.clone(), cx);
    windows::wire_relaunch_restore(!launch.initial_urls.is_empty(), cx);
    if !launch.initial_urls.is_empty() {
        let urls = launch.initial_urls.clone();
        main.update(cx, |mw, _, cx| mw.handle_deep_links(urls, cx)).ok();
    }
    for rx in [launch.forwarded.clone(), launch.open_rx.clone()] {
        cx.spawn(async move |cx| {
            while let Ok(urls) = rx.recv_async().await {
                let urls: Vec<String> = urls.into_iter().filter(|u| u.starts_with("lookout://")).collect();
                cx.update(|cx| {
                    windows::focus_main(cx);
                    if let Some(h) = cx.try_global::<windows::MainHandle>().map(|h| h.0) {
                        h.update(cx, |mw, _, cx| {
                            if !urls.is_empty() {
                                mw.handle_deep_links(urls, cx)
                            }
                        })
                        .ok();
                    }
                });
            }
        })
        .detach();
    }
}
