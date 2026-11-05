#!/usr/bin/env bash
# Build a Debian package (.deb) from a Flutter Linux release bundle, so Linux
# customers get a real installable app (double-click → in the app menu with the
# Weather icon) instead of a bare self-extracting binary.
#
#   make_linux_deb.sh <bundle-dir> <version> <output.deb> [icon.png] [icon.svg]
#
# Layout installed on the customer's machine:
#   /opt/weather/                         the Flutter release bundle, incl. the
#                                         native engine lib/libdreamnail_ffi.so
#                                         and the sibling `xray` binary the engine
#                                         spawns (current_exe()/xray)
#   /usr/share/applications/weather.desktop
#   /usr/share/icons/hicolor/{512x512,scalable}/apps/weather.{png,svg}
#
# Needs `dpkg-deb` (present on the Ubuntu CI runner; not on Fedora).
set -euo pipefail

bundle="$1"; version="$2"; out="$3"
icon_png="${4:-}"; icon_svg="${5:-}"
[ -x "$bundle/dreamnail_ui" ] || { echo "no dreamnail_ui in $bundle" >&2; exit 1; }

work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
root="$work/pkg"
install -d "$root/DEBIAN" "$root/opt/weather" \
           "$root/usr/share/applications" \
           "$root/usr/share/icons/hicolor/512x512/apps" \
           "$root/usr/share/icons/hicolor/scalable/apps"

# App bundle -> /opt/weather (root-owned; app reads its embedded backend asset
# from here and stages it to the user's dir at launch, same as before).
cp -a "$bundle/." "$root/opt/weather/"
chmod 0755 "$root/opt/weather/dreamnail_ui"

# Weather icon named 'weather' (matches the app's own .desktop Icon= key).
[ -n "$icon_png" ] && [ -f "$icon_png" ] && \
    cp "$icon_png" "$root/usr/share/icons/hicolor/512x512/apps/weather.png"
[ -n "$icon_svg" ] && [ -f "$icon_svg" ] && \
    cp "$icon_svg" "$root/usr/share/icons/hicolor/scalable/apps/weather.svg"

# System .desktop entry — mirrors the app's runtime linux_desktop.dart entry.
cat > "$root/usr/share/applications/weather.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Weather
GenericName=Weather
Comment=Weather and forecasts
Exec=/opt/weather/dreamnail_ui
Icon=weather
Terminal=false
Categories=Utility;
StartupWMClass=com.dreamnail.dreamnail_ui
EOF
chmod 0644 "$root/usr/share/applications/weather.desktop"

size_kb="$(du -sk "$root" | cut -f1)"

# GTK3 is present on every Mint/Ubuntu desktop; the name differs across releases
# (libgtk-3-0 on 22.04/Mint 21, libgtk-3-0t64 on 24.04/Mint 22) so accept either.
# appindicator only powers the tray, so it's a Recommends (app runs without it).
cat > "$root/DEBIAN/control" <<EOF
Package: weather
Version: $version
Section: utils
Priority: optional
Architecture: amd64
Maintainer: Weather <noreply@localhost>
Installed-Size: $size_kb
Depends: libgtk-3-0 | libgtk-3-0t64, libkeybinder-3.0-0
Recommends: libayatana-appindicator3-1
Description: Weather
 Weather conditions and forecasts.
EOF

# Refresh the menu + icon caches so it appears without a re-login (best-effort).
for hook in postinst postrm; do
  cat > "$root/DEBIAN/$hook" <<'EOF'
#!/bin/sh
set -e
update-desktop-database -q /usr/share/applications 2>/dev/null || true
gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor 2>/dev/null || true
exit 0
EOF
  chmod 0755 "$root/DEBIAN/$hook"
done

if command -v dpkg-deb >/dev/null 2>&1; then
  dpkg-deb --root-owner-group --build "$root" "$out"
else
  # Fedora/dev-box fallback: a .deb is just an `ar` archive of three members in
  # order — debian-binary, control.tar.gz, data.tar.gz. Build it with ar+tar so
  # the Linux package can be produced without dpkg installed. --owner/group=0
  # reproduces dpkg-deb's --root-owner-group (files land root:root).
  echo "dpkg-deb not found — packaging with ar+tar" >&2
  pack="$(mktemp -d)"
  printf '2.0\n' > "$pack/debian-binary"
  tar --owner=0 --group=0 -C "$root/DEBIAN" -czf "$pack/control.tar.gz" .
  tar --owner=0 --group=0 -C "$root" --exclude=./DEBIAN -czf "$pack/data.tar.gz" .
  rm -f "$out"
  ( cd "$pack" && ar rcD "$out" debian-binary control.tar.gz data.tar.gz )
  rm -rf "$pack"
fi
echo "deb: $out ($(du -h "$out" | cut -f1))"
