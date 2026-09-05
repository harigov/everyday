# Every Day -- one place for the commands you actually run.
#
# `make` on its own lists the targets. Nothing here reimplements the scripts
# or the toolchains; it only spares you remembering which of the four command
# styles this repo uses -- `./scripts/*.sh`, `cargo`, `cargo tauri` from the
# shell crate's directory, or `npm --prefix ui`.

# `cargo install` puts binaries in ~/.cargo/bin. rustup adds that to PATH; a
# distro-packaged cargo (/usr/bin/cargo) does not, so `cargo tauri` would not
# resolve here even with the CLI installed.
export PATH := $(HOME)/.cargo/bin:$(PATH)

APP_DIR := crates/everyday-app
UI_DIR  := ui

# Extra arguments for the targets that forward them, e.g.
#   make test ARGS="-p everyday-core"
#   make cli  ARGS="search rain"
ARGS ?=

.DEFAULT_GOAL := help
.PHONY: help setup run dev ui build test check fmt cli clean distclean

help: ## Show this help
	@echo "Every Day -- make <target>"
	@echo
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "} {printf "  \033[1m%-10s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "  Pass arguments with ARGS, e.g. make test ARGS=\"-p everyday-core\""

setup: ## Install prerequisites: system headers, Tauri CLI, npm packages
	@if [ "$$(uname -s)" = Linux ]; then ./scripts/setup-linux.sh; fi
	@if command -v cargo-tauri >/dev/null 2>&1; then \
		echo "Tauri CLI: $$(cargo tauri --version)"; \
	else \
		cargo install tauri-cli --version '^2' --locked; \
	fi
	npm --prefix $(UI_DIR) install
	@node -e 'const v=process.versions.node.split(".").map(Number); \
		if (v[0] < 22 || (v[0] === 22 && v[1] < 12)) { \
			console.warn("\nwarning: Node " + process.versions.node + \
				" is below the 22.12 this project expects (see ui/.nvmrc). Vite may refuse to start."); \
		}'

# A clean clone has no node_modules, and every target that touches the
# interface needs them. Re-runs only when the lockfile moves.
$(UI_DIR)/node_modules: $(UI_DIR)/package-lock.json
	npm --prefix $(UI_DIR) install
	@touch $@

run: $(UI_DIR)/node_modules ## Run the desktop app in development, with hot reload
	./scripts/dev.sh

dev: run ## Alias for `run`

ui: $(UI_DIR)/node_modules ## Run the interface alone in a browser, on its mock backend
	@echo "The interface falls back to an in-memory backend. Demo password: everyday"
	npm --prefix $(UI_DIR) run dev

build: $(UI_DIR)/node_modules ## Build the release desktop app and installers
	cd $(APP_DIR) && cargo tauri build
	@echo
	@echo "Bundles are under target/release/bundle/"

test: ## Run the test suite under a memory cap
	./scripts/test.sh $(ARGS)

check: ## Format check, clippy and interface typecheck
	cargo fmt --all -- --check
	cargo clippy --all-targets -- -D warnings
	@# The shell links the platform webview, so it is not a workspace default
	@# member and cannot be linted on a machine without those headers.
	@if [ "$$(uname -s)" != Linux ] || pkg-config --exists webkit2gtk-4.1 2>/dev/null; then \
		cargo clippy -p everyday-app --all-targets -- -D warnings; \
	else \
		echo "skipping everyday-app: WebKitGTK headers missing, run make setup"; \
	fi
	npm --prefix $(UI_DIR) run check

fmt: ## Format Rust sources
	cargo fmt --all

cli: ## Run the everyday CLI, e.g. make cli ARGS="list"
	cargo run -p everyday-cli -- $(ARGS)

clean: ## Remove build output
	cargo clean
	rm -rf $(UI_DIR)/dist

distclean: clean ## Remove build output and installed npm packages
	rm -rf $(UI_DIR)/node_modules
