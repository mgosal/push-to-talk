APP_NAME   := Push to Talk
BUNDLE     := $(APP_NAME).app
BINARY     := push-to-talk
IDENTIFIER := com.mgosal.push-to-talk
INSTALL_DIR := /Applications
MACOSX_DEPLOYMENT_TARGET := 14.0
export MACOSX_DEPLOYMENT_TARGET

.PHONY: check test build bundle install uninstall clean

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
	@# Ad-hoc sign so macOS gives it a stable identity for TCC
	@codesign --force --sign - --deep "$(BUNDLE)"
	@echo "✓ $(BUNDLE) ready (ad-hoc signed)"

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
