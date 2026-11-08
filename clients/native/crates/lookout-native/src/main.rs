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
        let main = windows::main_window::open(cx);
        smoke::start(cx);
        smoke::start_tour(cx);
        cx.set_global(windows::MainHandle(main));
        windows::wire_tray(rt.clone(), cx);
        windows::wire_relaunch_restore(!initial_urls.is_empty(), cx);
        if !initial_urls.is_empty() {
            let urls = initial_urls.clone();
            main.update(cx, |mw, _, cx| mw.handle_deep_links(urls, cx)).ok();
        }
        for rx in [forwarded.clone(), open_rx.clone()] {
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
    });
    crash::on_exit();
    drop(runtime);
}
