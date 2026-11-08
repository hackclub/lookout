/* Lookout recording indicator.
 *
 * GNOME owns its top bar: there is no D-Bus API for an app to add a panel
 * item, and the pill the shell shows for its own screen recorder
 * (`screen-recording-indicator`) is internal to it. So the pill has to be
 * drawn from inside the shell — which is what this extension is for. It
 * reuses that same style class, so it picks up whatever the active theme
 * (Adwaita, Yaru, …) paints the shell's recorder pill with.
 *
 * All state comes from the app over the session bus; this extension holds
 * none of its own and shows nothing when Lookout isn't running. The app
 * pushes a StateChanged every second while recording, so the label never
 * ticks a clock of its own — no drift against the app's own timer.
 *
 * Plain Gio.DBusProxy on purpose, rather than makeProxyWrapper: the wrapper's
 * calling convention has shifted between gjs versions, and this file has to
 * load unchanged across GNOME 45 through 50.
 */

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {Extension, gettext as _} from 'resource:///org/gnome/shell/extensions/extension.js';

// Under the app's Tauri identifier (`com.hackclub.lookout`), because that is
// what a sandboxed build is allowed to own — see BUS_NAME in
// src/gnome_indicator.rs. The path and interface keep their old spelling on
// purpose: a sandbox filters names, not paths, so renaming them would have
// broken older copies of this file for nothing.
//
// The app also still owns the pre-1.2.0 name, `com.hackclub.Lookout`, so a
// shell running that older copy keeps its pill until the next login.
const BUS_NAME = 'com.hackclub.lookout.Indicator';
const OBJECT_PATH = '/com/hackclub/Lookout/Indicator';
const IFACE_NAME = 'com.hackclub.Lookout.Indicator';

const LookoutIndicator = GObject.registerClass(
class LookoutIndicator extends PanelMenu.Button {
    _init(delegate) {
        super._init(0.5, 'Lookout', false);

        this._delegate = delegate;

        // The shell's own recording pill styling. `panel-button` comes from
        // PanelMenu.Button; this is the class that turns it into a pill.
        this.add_style_class_name('screen-recording-indicator');

        // Our own class carries the width trim and the paused fill; see
        // stylesheet.css.
        this.add_style_class_name('lookout-indicator');

        this._appIcon = delegate.appIcon;
        this._pausedIcon = new Gio.ThemedIcon({
            name: 'media-playback-pause-symbolic',
        });

        const box = new St.BoxLayout({style_class: 'lookout-indicator-box'});
        // gicon in both states rather than icon_name in one and gicon in the
        // other: setting icon_name leaves a themed gicon behind, and the two
        // properties then disagree about which wins.
        this._icon = new St.Icon({
            gicon: this._appIcon,
            style_class: 'lookout-indicator-icon',
        });
        this._label = new St.Label({
            style_class: 'lookout-indicator-label',
            y_align: Clutter.ActorAlign.CENTER,
            text: '',
        });
        box.add_child(this._icon);
        box.add_child(this._label);
        this.add_child(box);

        this._toggleItem = new PopupMenu.PopupMenuItem(_('Pause recording'));
        this._toggleItem.connect('activate', () => this._delegate.toggle());
        this.menu.addMenuItem(this._toggleItem);

        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        const stopItem = new PopupMenu.PopupMenuItem(_('Stop recording'));
        stopItem.connect('activate', () => this._delegate.stop());
        this.menu.addMenuItem(stopItem);

        const openItem = new PopupMenu.PopupMenuItem(_('Open Lookout'));
        openItem.connect('activate', () => this._delegate.open());
        this.menu.addMenuItem(openItem);
    }

    setState(time, paused) {
        this._label.text = time;
        this._toggleItem.label.text = paused
            ? _('Resume recording')
            : _('Pause recording');
        // Paused is not recording: amber fill and a pause glyph, so a paused
        // session doesn't sit in the panel claiming to be live. Recording
        // keeps the shell's own red and shows the app's icon.
        if (paused) {
            this.remove_style_class_name('screen-recording-indicator');
            this.add_style_class_name('lookout-indicator-paused');
            this._icon.gicon = this._pausedIcon;
        } else {
            this.remove_style_class_name('lookout-indicator-paused');
            this.add_style_class_name('screen-recording-indicator');
            this._icon.gicon = this._appIcon;
        }
    }
});

export default class LookoutIndicatorExtension extends Extension {
    enable() {
        this._indicator = null;
        this._proxy = null;
        this._signalId = 0;
        this._cancellable = new Gio.Cancellable();

        // Watching the name rather than connecting once means the pill
        // survives the app restarting, and costs nothing while it is closed.
        this._watchId = Gio.bus_watch_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameWatcherFlags.NONE,
            () => this._connect(),
            () => this._disconnect());
    }

    disable() {
        this._detach();
        this._disconnect();

        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = 0;
        }

        this._cancellable?.cancel();
        this._cancellable = null;
    }

    _connect() {
        Gio.DBusProxy.new(
            Gio.DBus.session,
            Gio.DBusProxyFlags.DO_NOT_AUTO_START,
            null,
            BUS_NAME,
            OBJECT_PATH,
            IFACE_NAME,
            this._cancellable,
            (_obj, res) => {
                let proxy;
                try {
                    proxy = Gio.DBusProxy.new_finish(res);
                } catch (e) {
                    if (!e.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                        console.error(`Lookout indicator: ${e.message}`);
                    return;
                }

                this._proxy = proxy;
                this._signalId = proxy.connect('g-signal',
                    (_p, _sender, signal, params) => {
                        if (signal === 'StateChanged')
                            this._onState(...params.deepUnpack());
                    });

                // Tells the app to stand down its StatusNotifierItem: with the
                // pill up, a tray icon would be a second indicator for the
                // same recording.
                this._call('Attach');
                this._call('GetState', (value) => this._onState(...value));
            });
    }

    _disconnect() {
        if (this._proxy && this._signalId) {
            this._proxy.disconnect(this._signalId);
            this._signalId = 0;
        }
        this._proxy = null;
        this._onState(false, '', false);
    }

    /** Best-effort Detach so the app can put its tray back if we're disabled. */
    _detach() {
        if (this._proxy)
            this._call('Detach');
    }

    _call(method, onReply = null) {
        this._proxy?.call(method, null, Gio.DBusCallFlags.NONE, -1,
            this._cancellable, (proxy, res) => {
                try {
                    const reply = proxy.call_finish(res).deepUnpack();
                    onReply?.(reply);
                } catch (e) {
                    if (!e.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                        console.error(`Lookout indicator: ${method}: ${e.message}`);
                }
            });
    }

    _onState(active, time, paused) {
        if (!active) {
            this._indicator?.destroy();
            this._indicator = null;
            return;
        }

        if (!this._indicator) {
            this._indicator = new LookoutIndicator({
                // Shipped beside this file by the app's installer, so it is
                // found whether the app came from a deb, an rpm or the
                // AppImage.
                appIcon: Gio.FileIcon.new(Gio.File.new_for_path(
                    GLib.build_filenamev([this.path, 'icons', 'lookout.png']))),
                toggle: () => this._call(this._paused ? 'Resume' : 'Pause'),
                stop: () => this._call('Stop'),
                open: () => this._call('Open'),
            });
            // Position 0 of the right box puts it left of the system menu,
            // where the shell's own recording pill sits.
            Main.panel.addToStatusArea(this.uuid, this._indicator, 0, 'right');
        }

        this._paused = paused;
        this._indicator.setState(time, paused);
    }
}
