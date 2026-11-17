#!/bin/sh
# W30 (G5g): OpenTTD on VEILOS. OpenTTD and its graphics aren't in the
# repository: this fetches them from openttd.org into DIR (checking the
# published SHA-256 sums), unpacks them, adds the test's settings and start
# script, and makes the Windows reference runs in DIR/ref:
#
#   headless   -v null:ticks=300 -s null -m null -G 1234 -g, saving on exit
#   window     -v win32 (GDI) -s null -m null -G 1234 -g -r 640x480, paused,
#              the start script taking two screenshots that render the game
#              (the zoomed-in view and the minimap), not the screen
#
# Then put DIR/openttd on the VEILOS disk (/games/openttd) and run the same
# command lines there (the headless one also with -s win32, waveOut, on an
# HD Audio device). With the window open 45 s, press Ctrl+S (OpenTTD's
# screenshot of its screen) and take the VM's screendump. Fetch exit.sav and
# the screenshot folder (the Ctrl+S one as late.png) and compare with
# check.py REF GOT SCREENDUMP.
#
#   sh win32/tests/w30/prepare.sh DIR
set -e
DIR=${1:?usage: prepare.sh DIR}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$DIR/dl" "$DIR/ref"
cd "$DIR/dl"
fetch() {
    [ -f "$2" ] || curl -sSfL -o "$2" "$1"
    echo "$3 *$2" | sha256sum -c -
}
fetch https://cdn.openttd.org/openttd-releases/15.3/openttd-15.3-windows-win64.zip openttd-15.3-windows-win64.zip 61c0a6a43d81008c7ff4330fb56351bbf66a980cdad041f1d0e08b51f2eeb34c
fetch https://cdn.openttd.org/opengfx-releases/8.0/opengfx-8.0-all.zip opengfx-8.0-all.zip 43a0c1dabf39cb865394f3a6cc36d4da5c10ecfaaf55652043104806810903be
rm -rf "$DIR/unpack" "$DIR/openttd"
mkdir -p "$DIR/unpack"
(cd "$DIR/unpack" && unzip -q ../dl/openttd-15.3-windows-win64.zip && unzip -q ../dl/opengfx-8.0-all.zip)
cp -r "$DIR/unpack/openttd-15.3-windows-win64" "$DIR/openttd"
cp "$DIR/unpack/opengfx-8.0.tar" "$DIR/openttd/baseset/"
cp "$HERE/test.cfg" "$HERE/gui.cfg" "$DIR/openttd/"
cp "$HERE/game_start.scr" "$DIR/openttd/scripts/"

# The reference runs, on Windows.
cd "$DIR/openttd"
rm -rf save screenshot
./openttd.exe -X -x -c test.cfg -v null:ticks=300 -s null -m null -G 1234 -g
cp save/autosave/exit.sav "$DIR/ref/exit.sav"
# The window stays open (paused) after its screenshots: closed after 20 s.
timeout 20 ./openttd.exe -X -x -c gui.cfg -v win32 -s null -m null -G 1234 -g -r 640x480 || true
cp screenshot/bg.png screenshot/mm.png "$DIR/ref/"
rm -rf save screenshot
echo "prepared: $DIR/openttd, references in $DIR/ref"
