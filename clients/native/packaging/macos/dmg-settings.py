# dmgbuild settings for the Lookout installer disk image. bundle.sh passes
# the paths with -D; the window is laid out for assets/installer/dmg-background.png
# (2000x1041, drawn at 560x291 points): the app sits left of the arrow,
# Applications to its right.
#
# window_rect is the whole window frame, title bar included, so the frame is
# the artwork's height plus TITLE_BAR. Title bars differ between macOS
# versions (about 28pt before Tahoe, 32pt on it). Finder stretches the
# background to fill the content area, so a shorter title bar just stretches
# the art a few points taller; it never uncovers white.
import os.path

app = defines["app"]  # noqa: F821 (dmgbuild injects `defines`)
app_name = os.path.basename(app)

format = "UDZO"
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["volume_icon"]  # noqa: F821
background = defines["background"]  # noqa: F821

CONTENT_W, CONTENT_H = 560, 291
TITLE_BAR = 32
window_rect = ((200, 120), (CONTENT_W, CONTENT_H + TITLE_BAR))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
arrange_by = None
icon_size = 112
text_size = 13
icon_locations = {
    app_name: (140, 140),
    "Applications": (420, 140),
}
