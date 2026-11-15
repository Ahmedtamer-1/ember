#!/usr/bin/env bash
set -euo pipefail

# Ember Browser — Linux Portable & System Package Generator
VERSION="${1:-0.1.0}"
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${PROJECT_ROOT}/target/dist"
PKG_NAME="ember-v${VERSION}-linux-x64"
PKG_DIR="${DIST_DIR}/${PKG_NAME}"

echo "=== Building Ember Browser for Linux (v${VERSION}) ==="
cd "${PROJECT_ROOT}"

if [ ! -f "target/release/ember" ]; then
    echo "Running cargo build --release..."
    cargo build --release
fi

echo "Creating Linux package directory structure..."
rm -rf "${PKG_DIR}"
mkdir -p "${PKG_DIR}/bin"
mkdir -p "${PKG_DIR}/share/applications"
mkdir -p "${PKG_DIR}/share/icons/hicolor/scalable/apps"

echo "Copying binary and assets..."
cp "target/release/ember" "${PKG_DIR}/bin/ember"
chmod +x "${PKG_DIR}/bin/ember"

if [ -f "assets/ember.svg" ]; then
    cp "assets/ember.svg" "${PKG_DIR}/share/icons/hicolor/scalable/apps/ember.svg"
fi

cp "README.md" "${PKG_DIR}/" 2>/dev/null || true
cp "BENCHMARKS.md" "${PKG_DIR}/" 2>/dev/null || true
cp "LICENSE-MIT" "${PKG_DIR}/" 2>/dev/null || true
cp "LICENSE-APACHE" "${PKG_DIR}/" 2>/dev/null || true

# Generate Desktop Entry
cat << 'EOF' > "${PKG_DIR}/share/applications/ember.desktop"
[Desktop Entry]
Version=1.0
Type=Application
Name=Ember
GenericName=Web Browser
Comment=Extremely lightweight desktop web browser written in Rust
Exec=ember %u
Icon=ember
Terminal=false
Categories=Network;WebBrowser;
MimeType=text/html;text/xml;application/xhtml+xml;x-scheme-handler/http;x-scheme-handler/https;x-scheme-handler/ember;
StartupNotify=true
StartupWMClass=ember
EOF

# Generate Portable Launcher
cat << 'EOF' > "${PKG_DIR}/ember"
#!/usr/bin/env bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${SCRIPT_DIR}/bin/ember" "$@"
EOF
chmod +x "${PKG_DIR}/ember"

# Generate One-Click Installer Script
cat << 'EOF' > "${PKG_DIR}/install.sh"
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ "$(id -u)" -eq 0 ]; then
    BIN_DIR="/usr/local/bin"
    APP_DIR="/usr/share/applications"
    ICON_DIR="/usr/share/icons/hicolor/scalable/apps"
else
    BIN_DIR="${HOME}/.local/bin"
    APP_DIR="${HOME}/.local/share/applications"
    ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"
fi

mkdir -p "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}"

echo "Installing Ember binary to ${BIN_DIR}/ember..."
cp "${SCRIPT_DIR}/bin/ember" "${BIN_DIR}/ember"
chmod +x "${BIN_DIR}/ember"

echo "Installing desktop launcher to ${APP_DIR}/ember.desktop..."
cp "${SCRIPT_DIR}/share/applications/ember.desktop" "${APP_DIR}/ember.desktop"

if [ -f "${SCRIPT_DIR}/share/icons/hicolor/scalable/apps/ember.svg" ]; then
    echo "Installing application icon to ${ICON_DIR}/ember.svg..."
    cp "${SCRIPT_DIR}/share/icons/hicolor/scalable/apps/ember.svg" "${ICON_DIR}/ember.svg"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${APP_DIR}" 2>/dev/null || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$(dirname "${ICON_DIR}")/.." 2>/dev/null || true
fi

echo "Ember successfully installed!"
echo "Make sure ${BIN_DIR} is in your PATH. You can now launch Ember by running 'ember' or selecting it from your application menu."
EOF
chmod +x "${PKG_DIR}/install.sh"

echo "Creating tar.gz distribution archive..."
cd "${DIST_DIR}"
tar -czf "${PKG_NAME}.tar.gz" "${PKG_NAME}"

echo "Linux package created successfully at: ${DIST_DIR}/${PKG_NAME}.tar.gz"

