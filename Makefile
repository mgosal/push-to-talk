APP_NAME   := Push to Talk
BUNDLE     := $(APP_NAME).app
BINARY     := push-to-talk
IDENTIFIER := com.mgosal.push-to-talk
INSTALL_DIR := /Applications
# Created once by `make signing-cert`. Without it the bundle is ad-hoc signed,
# and macOS forgets its Accessibility and Microphone grants on every rebuild.
SIGN_IDENTITY := Push to Talk Local Signing
MACOSX_DEPLOYMENT_TARGET := 14.0
export MACOSX_DEPLOYMENT_TARGET

.PHONY: check test build bundle install uninstall clean signing-cert

# ── Build ──────────────────────────────────────────────────────────────

check:
	cargo check

test:
	cargo test

build:
	cargo build --release

# ── Bundle ─────────────────────────────────────────────────────────────

bundle: build
	@echo "Creating $(BUNDLE)..."
	@mkdir -p "$(BUNDLE)/Contents/MacOS"
	@mkdir -p "$(BUNDLE)/Contents/Resources"
	@cp bundle/Info.plist "$(BUNDLE)/Contents/"
	@cp target/release/$(BINARY) "$(BUNDLE)/Contents/MacOS/"
	@if [ -f bundle/AppIcon.icns ]; then \
		cp bundle/AppIcon.icns "$(BUNDLE)/Contents/Resources/"; \
	fi
	@# A certificate signature keeps the same identity across rebuilds, so
	@# privacy grants survive; an ad-hoc one is tied to this exact binary.
	@if security find-certificate -c "$(SIGN_IDENTITY)" >/dev/null 2>&1; then \
		codesign --force --sign "$(SIGN_IDENTITY)" --identifier "$(IDENTIFIER)" "$(BUNDLE)" || exit 1; \
		echo "✓ $(BUNDLE) ready (signed as \"$(SIGN_IDENTITY)\")"; \
	else \
		codesign --force --sign - --identifier "$(IDENTIFIER)" "$(BUNDLE)" || exit 1; \
		echo "✓ $(BUNDLE) ready (ad-hoc signed — permissions reset on every rebuild;"; \
		echo "  run 'make signing-cert' once to fix that)"; \
	fi

signing-cert:
	@tools/create-signing-cert.sh "$(SIGN_IDENTITY)"

# ── Install ────────────────────────────────────────────────────────────

install: bundle
	@echo "Installing to $(INSTALL_DIR)/$(BUNDLE)..."
	@rm -rf "$(INSTALL_DIR)/$(BUNDLE)"
	@cp -R "$(BUNDLE)" "$(INSTALL_DIR)/"
	@echo "✓ Installed. Launch from /Applications or:"
	@echo "  open '/Applications/$(APP_NAME).app'"
	@echo ""
	@echo "First launch: complete API key and permission setup from the setup window."

# ── Uninstall ──────────────────────────────────────────────────────────

uninstall:
	@rm -rf "$(INSTALL_DIR)/$(BUNDLE)"
	@echo "✓ Removed from $(INSTALL_DIR)"

# ── Clean ──────────────────────────────────────────────────────────────

clean:
	cargo clean
	@rm -rf "$(BUNDLE)"
