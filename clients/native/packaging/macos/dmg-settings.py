# dmgbuild settings for the Lookout installer disk image. bundle.sh passes
# the paths with -D; the window is laid out for assets/installer/dmg-background.png
# (2000x1041, drawn at 1000x520 points): the app sits left of the arrow,
# Applications to its right.
import os.path

app = defines["app"]  # noqa: F821 (dmgbuild injects `defines`)
app_name = os.path.basename(app)

format = "UDZO"
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["volume_icon"]  # noqa: F821
background = defines["background"]  # noqa: F821

window_rect = ((200, 120), (1000, 520))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
arrange_by = None
icon_size = 128
text_size = 14
icon_locations = {
    app_name: (250, 250),
    "Applications": (750, 250),
}
