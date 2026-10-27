#!/usr/bin/env python3
"""Copy the menus of ui/menus.slint's MainMenu into ui/app.slint's macOS
MenuBar.

Slint builds a MenuBar from the Menu elements written directly inside it,
before components are inlined, so the menu bar can't reuse MainMenu and gets
a copy instead. Run this after editing the menus; the test
`menu_bar_matches_the_menu` fails while the copy is out of date.
"""
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BEGIN = "        // BEGIN tools/menubar.py\n"
END = "        // END tools/menubar.py\n"


def menus(source: str) -> str:
    """The Menu elements inside MainMenu's root Menu, reading Playback and
    Media live: the menu bar is native and follows them, where the
    application menu reads the copy in MenuPlayback and MenuMedia."""
    start = source.index("export component MainMenu")
    start = source.index("    Menu {\n", start) + len("    Menu {\n")
    end = source.index("\n    }\n}", start)
    return (
        source[start:end]
        .replace("MenuMedia.playlist-count", "Media.playlist.length")
        .replace("MenuPlayback.", "Playback.")
        .replace("MenuMedia.", "Media.")
        + "\n"
    )


def main() -> int:
    menu_file = ROOT / "ui" / "menus.slint"
    app_file = ROOT / "ui" / "app.slint"
    app = app_file.read_text()
    begin = app.index(BEGIN) + len(BEGIN)
    end = app.index(END)
    updated = app[:begin] + menus(menu_file.read_text()) + app[end:]
    if updated != app:
        app_file.write_text(updated)
        print("updated the menu bar in ui/app.slint")
    else:
        print("the menu bar in ui/app.slint is up to date")
    return 0


if __name__ == "__main__":
    sys.exit(main())
