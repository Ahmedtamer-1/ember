#!/usr/bin/env bash
set -euo pipefail

# Ember Browser — Cross-platform Packaging Dispatcher
OS="$(uname -s)"

case "${OS}" in
    Darwin*)
        echo "Detected macOS system. Invoking scripts/package_macos.sh..."
        bash "$(dirname "$0")/package_macos.sh" "$@"
        ;;
    Linux*)
        echo "Detected Linux system. Invoking scripts/package_linux.sh..."
        bash "$(dirname "$0")/package_linux.sh" "$@"
        ;;
    MINGW*|MSYS*|CYGWIN*)
        echo "Detected Windows environment. Invoking scripts/package_windows.ps1..."
        powershell -ExecutionPolicy Bypass -File "$(dirname "$0")/package_windows.ps1" "$@"
        ;;
    *)
        echo "Unsupported OS: ${OS}"
        exit 1
        ;;
esac

