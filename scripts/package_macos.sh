#!/usr/bin/env bash
set -euo pipefail

# Ember Browser — macOS Application Bundle & Package Generator
VERSION="${1:-0.1.0}"
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${PROJECT_ROOT}/target/dist"
PKG_NAME="ember-v${VERSION}-macos-x64"
PKG_DIR="${DIST_DIR}/${PKG_NAME}"
APP_BUNDLE="${PKG_DIR}/Ember.app"

echo "=== Building Ember Browser for macOS (v${VERSION}) ==="
cd "${PROJECT_ROOT}"

if [ ! -f "target/release/ember" ]; then
    echo "Running cargo build --release..."
    cargo build --release
fi

echo "Creating macOS .app bundle directory structure..."
rm -rf "${PKG_DIR}"
mkdir -p "${APP_BUNDLE}/Contents/MacOS"
mkdir -p "${APP_BUNDLE}/Contents/Resources"

echo "Copying release binary and metadata..."
cp "target/release/ember" "${APP_BUNDLE}/Contents/MacOS/ember"
chmod +x "${APP_BUNDLE}/Contents/MacOS/ember"

if [ -f "scripts/Info.plist" ]; then
    cp "scripts/Info.plist" "${APP_BUNDLE}/Contents/Info.plist"
fi

if [ -f "assets/ember.svg" ]; then
    cp "assets/ember.svg" "${APP_BUNDLE}/Contents/Resources/ember.svg"
fi

cp "README.md" "${PKG_DIR}/" 2>/dev/null || true
cp "BENCHMARKS.md" "${PKG_DIR}/" 2>/dev/null || true
cp "LICENSE-MIT" "${PKG_DIR}/" 2>/dev/null || true
cp "LICENSE-APACHE" "${PKG_DIR}/" 2>/dev/null || true

# Generate one-click install script
cat << 'EOF' > "${PKG_DIR}/install.sh"
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_DIR="/Applications"

if [ ! -w "$TARGET_DIR" ]; then
    TARGET_DIR="$HOME/Applications"
    mkdir -p "$TARGET_DIR"
fi

echo "Installing Ember.app to ${TARGET_DIR}..."
rm -rf "${TARGET_DIR}/Ember.app"
cp -R "${SCRIPT_DIR}/Ember.app" "${TARGET_DIR}/"

echo "Ember successfully installed to ${TARGET_DIR}/Ember.app!"
echo "You can launch it from Spotlight, Launchpad, or by running:"
echo "  open ${TARGET_DIR}/Ember.app"
EOF
chmod +x "${PKG_DIR}/install.sh"

echo "Creating tar.gz distribution archive..."
cd "${DIST_DIR}"
tar -czf "${PKG_NAME}.tar.gz" "${PKG_NAME}"
zip -r "${PKG_NAME}.zip" "${PKG_NAME}" >/dev/null 2>&1 || true

# Generate DMG if hdiutil is available
if command -v hdiutil >/dev/null 2>&1; then
    echo "Generating macOS disk image (.dmg)..."
    hdiutil create -volname "Ember Browser" -srcfolder "${PKG_DIR}" -ov -format UDZO "${DIST_DIR}/${PKG_NAME}.dmg" || true
fi

echo "macOS package created successfully at: ${DIST_DIR}/${PKG_NAME}.tar.gz"

