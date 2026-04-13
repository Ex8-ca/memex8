#!/usr/bin/env bash
# Install memex8 binary to ~/.memex8/bin/memex8
# Usage: cargo run --release --bin install
#   or:  ./scripts/install.sh

set -euo pipefail

INSTALL_DIR="${HOME}/.memex8/bin"
BINARY="memex8"

echo "🔧 Installing ${BINARY} to ${INSTALL_DIR}..."

# Find the binary
if [ -f "target/release/${BINARY}" ]; then
    SRC="target/release/${BINARY}"
elif command -v "${BINARY}" &>/dev/null; then
    SRC="$(which "${BINARY}")"
else
    echo "❌ Binary not found. Run 'cargo build --release' first."
    exit 1
fi

# Create install directory
mkdir -p "${INSTALL_DIR}"

# Copy binary
cp "${SRC}" "${INSTALL_DIR}/${BINARY}"
chmod +x "${INSTALL_DIR}/${BINARY}"

# Add to PATH if not already there
SHELL_RC=""
case "${SHELL}" in
    */bash)  SHELL_RC="${HOME}/.bashrc" ;;
    */zsh)   SHELL_RC="${HOME}/.zshrc" ;;
    */fish)  SHELL_RC="${HOME}/.config/fish/config.fish" ;;
esac

if [ -n "${SHELL_RC}" ] && [ -f "${SHELL_RC}" ]; then
    if ! grep -q "\.memex8/bin" "${SHELL_RC}" 2>/dev/null; then
        echo "" >> "${SHELL_RC}"
        echo 'export PATH="${HOME}/.memex8/bin:${PATH}"' >> "${SHELL_RC}"
        echo "✅ Added ~/.memex8/bin to PATH in ${SHELL_RC}"
        echo "   Run 'source ${SHELL_RC}' or restart your terminal"
    fi
fi

echo ""
echo "✅ ${BINARY} installed to ${INSTALL_DIR}/${BINARY}"
echo ""
echo "Next steps:"
echo "  1. Set MEMEX8_API_KEY in ~/.memex8/.env"
echo "  2. Run: memex8 doctor"
echo "  3. Run: memex8 serve  (or docker compose up -d)"
