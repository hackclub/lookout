#!/usr/bin/env python3
"""Stand-in for the app's indicator service, for working on the extension.

Exports the same name, path, interface and signal `gnome_indicator.rs` does,
and ticks a second every second, so extension.js can be iterated without
rebuilding the desktop app. Prints the methods the extension calls back.

    python3 mock-service.py            # counts up from 0
    python3 mock-service.py --paused   # starts paused
"""

import sys

from gi.repository import Gio, GLib

BUS_NAME = "com.hackclub.Lookout"
OBJECT_PATH = "/com/hackclub/Lookout/Indicator"
IFACE_NAME = "com.hackclub.Lookout.Indicator"

XML = f"""
<node>
  <interface name="{IFACE_NAME}">
    <method name="GetState">
      <arg type="b" direction="out" name="active"/>
      <arg type="s" direction="out" name="time"/>
      <arg type="b" direction="out" name="paused"/>
    </method>
    <method name="Attach"/>
    <method name="Detach"/>
    <method name="Pause"/>
    <method name="Resume"/>
    <method name="Stop"/>
    <method name="Open"/>
    <signal name="StateChanged">
      <arg type="b" name="active"/>
      <arg type="s" name="time"/>
      <arg type="b" name="paused"/>
    </signal>
  </interface>
</node>
"""


def format_time(seconds):
    """Matches format_tray_time in lib.rs."""
    hours, rem = divmod(seconds, 3600)
    minutes, secs = divmod(rem, 60)
    if hours:
        return f"{hours}:{minutes:02}:{secs:02}"
    return f"{minutes:02}:{secs:02}"


class MockIndicator:
    def __init__(self, paused=False):
        self.active = True
        self.paused = paused
        self.seconds = 0
        self.connection = None
        self.node = Gio.DBusNodeInfo.new_for_xml(XML)
        Gio.bus_own_name(
            Gio.BusType.SESSION, BUS_NAME, Gio.BusNameOwnerFlags.NONE,
            self._on_acquired, None, self._on_lost,
        )

    def _on_acquired(self, connection, _name):
        self.connection = connection
        connection.register_object(
            OBJECT_PATH, self.node.interfaces[0], self._on_call, None, None,
        )
        print(f"owning {BUS_NAME}", flush=True)
        GLib.timeout_add_seconds(1, self._tick)

    def _on_lost(self, _connection, name):
        sys.exit(f"lost {name} — is the app (or another mock) already running?")

    def _state(self):
        return GLib.Variant("(bsb)", (self.active, format_time(self.seconds), self.paused))

    def _emit(self):
        self.connection.emit_signal(
            None, OBJECT_PATH, IFACE_NAME, "StateChanged", self._state(),
        )

    def _tick(self):
        if self.active and not self.paused:
            self.seconds += 1
        self._emit()
        return GLib.SOURCE_CONTINUE

    def _on_call(self, _conn, _sender, _path, _iface, method, _params, invocation):
        print(f"<- {method}", flush=True)
        if method == "GetState":
            invocation.return_value(self._state())
            return

        if method == "Pause":
            self.paused = True
        elif method == "Resume":
            self.paused = False
        elif method == "Stop":
            self.active = False

        invocation.return_value(None)
        if method in ("Pause", "Resume", "Stop"):
            self._emit()


if __name__ == "__main__":
    MockIndicator(paused="--paused" in sys.argv)
    GLib.MainLoop().run()
