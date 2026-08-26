#!/usr/bin/env bash
# setup-linux.sh — one-time setup for GitAgent on Linux (Debian/Ubuntu/Fedora/Arch)
#
# Installs: WebKitGTK (runtime), libxdo
# GitAgent itself shells out to `git` (required) and `gh` (only for the
# review/merge flow) rather than bundling them — this script only covers the
# GUI's own runtime libraries, and warns if git/gh are missing rather than
# installing them for you.
#
# Usage (from the extracted release archive):
#   chmod +x setup-linux.sh && sudo ./setup-linux.sh

set -e

info()  { echo -e "\033[34m[info]\033[0m  $*"; }
ok()    { echo -e "\033[32m[ok]\033[0m    $*"; }
skip()  { echo -e "\033[33m[skip]\033[0m  $*"; }
warn()  { echo -e "\033[33m[warn]\033[0m  $*"; }
err()   { echo -e "\033[31m[error]\033[0m $*"; exit 1; }

# ── Detect distro ─────────────────────────────────────────────────────────────
if   command -v apt-get &>/dev/null; then DISTRO=debian
elif command -v dnf     &>/dev/null; then DISTRO=fedora
elif command -v pacman  &>/dev/null; then DISTRO=arch
else err "Unsupported distro — install dependencies manually (see README)"
fi

info "Detected distro family: $DISTRO"

# ── Runtime dependencies (WebKitGTK + libxdo) ────────────────────────────────
case "$DISTRO" in
  debian)
    PKGS=()
    dpkg -l libwebkit2gtk-4.1-0 &>/dev/null || dpkg -l libwebkit2gtk-4.0-0 &>/dev/null || PKGS+=(libwebkit2gtk-4.1-0)
    dpkg -l libxdo3 &>/dev/null || PKGS+=(libxdo3)
    if [ ${#PKGS[@]} -gt 0 ]; then
      info "Installing runtime libs: ${PKGS[*]}"
      apt-get update -qq
      apt-get install -y "${PKGS[@]}" 2>/dev/null || apt-get install -y libwebkit2gtk-4.0-0 libxdo3
      ok "Runtime libs installed"
    else
      skip "Runtime libs already installed"
    fi
    ;;
  fedora)
    if ! rpm -q webkit2gtk4.1 &>/dev/null && ! rpm -q webkit2gtk3 &>/dev/null; then
      info "Installing WebKitGTK runtime..."
      dnf install -y webkit2gtk4.1 2>/dev/null || dnf install -y webkit2gtk3
      ok "WebKitGTK installed"
    else
      skip "WebKitGTK already installed"
    fi
    rpm -q xdotool &>/dev/null || dnf install -y xdotool
    ;;
  arch)
    if ! pacman -Qi webkit2gtk-4.1 &>/dev/null && ! pacman -Qi webkit2gtk &>/dev/null; then
      info "Installing WebKitGTK runtime..."
      pacman -S --noconfirm webkit2gtk-4.1 2>/dev/null || pacman -S --noconfirm webkit2gtk
      ok "WebKitGTK installed"
    else
      skip "WebKitGTK already installed"
    fi
    pacman -Qi xdotool &>/dev/null || pacman -S --noconfirm xdotool
    ;;
esac

# ── git / gh — GitAgent's own dependencies, not installed for you ───────────
if ! command -v git &>/dev/null; then
  warn "git not found on PATH — GitAgent needs it for every flow. Install it \
before running the app (e.g. 'apt install git')."
else
  ok "git found ($(git --version))"
fi

if ! command -v gh &>/dev/null; then
  warn "gh (GitHub CLI) not found — the Review → Merge flow needs it. \
Install it from https://cli.github.com if you use that flow; the \
Commit → PR flow works without it as long as origin isn't GitHub-only."
else
  ok "gh found ($(gh --version | head -1))"
fi

# ── Desktop shortcut (optional) ───────────────────────────────────────────────
BINARY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DESKTOP_FILE="/usr/share/applications/gitagent.desktop"

# Register the bundled icon with the hicolor theme so launchers pick it up.
# Falls back to the generic terminal glyph if the PNG isn't shipped.
ICON_NAME="utilities-terminal"
if [[ -f "$BINARY_DIR/icon.png" ]]; then
  ICON_DIR="/usr/share/icons/hicolor/256x256/apps"
  install -Dm644 "$BINARY_DIR/icon.png" "$ICON_DIR/gitagent.png"
  if command -v gtk-update-icon-cache &>/dev/null; then
    gtk-update-icon-cache -q -t /usr/share/icons/hicolor || true
  fi
  ICON_NAME="gitagent"
  ok "Icon installed to $ICON_DIR/gitagent.png"
fi

if [[ -f "$BINARY_DIR/gitagent" && ! -f "$DESKTOP_FILE" ]]; then
  info "Creating .desktop launcher..."
  cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Name=GitAgent
Comment=Agentic graph for commit and deploy workflows
Exec=$BINARY_DIR/gitagent
Icon=$ICON_NAME
Terminal=false
Type=Application
Categories=Development;
StartupWMClass=gitagent
EOF
  if command -v update-desktop-database &>/dev/null; then
    update-desktop-database -q /usr/share/applications || true
  fi
  ok "Desktop shortcut created"
fi

echo ""
echo "Setup complete. Run ./gitagent to start the app."
