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
.PHONY: help setup run dev ui build test test-postgres lint check fix fmt cli icons desktop-entry undesktop-entry clean distclean

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
	@# The shell is not a workspace default member -- it links the platform
	@# webview -- so a plain `cargo test` skips it, and its tests would
	@# otherwise run nowhere at all. That includes the one holding a feed's
	@# subscription URL, which is a bearer credential, out of error messages
	@# and logs. Skipped when ARGS names a package, and when the headers to
	@# build it are missing; CI installs them and runs it unconditionally.
	@if [ -z "$(ARGS)" ] && { [ "$$(uname -s)" != Linux ] || pkg-config --exists webkit2gtk-4.1 2>/dev/null; }; then \
		./scripts/test.sh -p everyday-app; \
	fi

# The storage conformance suite against a real Postgres, which `make test`
# cannot do because it would need a server it has no business starting. CI
# runs the same suite against a service container on every push; this is the
# way to run it before pushing. The container is removed when it stops.
test-postgres: ## Run the storage suite against a throwaway Postgres in Docker
	@docker rm -f everyday-pgtest >/dev/null 2>&1 || true
	docker run -d --rm --name everyday-pgtest \
		-e POSTGRES_PASSWORD=test -e POSTGRES_DB=everyday_test \
		-p 55432:5432 postgres:16-alpine
	@echo "waiting for Postgres..."
	@for i in $$(seq 1 60); do \
		docker exec everyday-pgtest pg_isready -q && break; \
		sleep 1; \
	done
	@EVERYDAY_TEST_DATABASE_URL=postgresql://postgres:test@127.0.0.1:55432/everyday_test \
		cargo test -p everyday-store-postgres; \
	status=$$?; \
	docker rm -f everyday-pgtest >/dev/null; \
	exit $$status

# The pair to reach for: `lint` says what is wrong, `fix` fixes what it can.
# They are the same tools in the same order, so anything `fix` silences is
# something `lint` would have complained about, and what survives a `fix` is
# the list of things that need a person.
#
# Between them, the two jobs in .github/workflows/check.yml run exactly what
# `lint` runs -- split in two only so the Rust half and the interface half go
# in parallel. Keep the two in step when you change either: a check you can
# only discover from a red build ten minutes after pushing is one people
# learn to ignore.

lint: $(UI_DIR)/node_modules ## Format check, clippy and interface typecheck -- changes nothing
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
	@# The generated client must be what the Rust command table says. The
	@# surface snapshot catches a change on the Rust side; this catches a
	@# generated file that was edited or never regenerated.
	npm --prefix $(UI_DIR) run gen:api
	@if ! git diff --quiet -- $(UI_DIR)/src/lib/generated; then \
		echo "the generated command client is out of date: run 'make fix' and commit it"; \
		git --no-pager diff --stat -- $(UI_DIR)/src/lib/generated; \
		exit 1; \
	fi

check: lint ## Alias for `lint`

fix: $(UI_DIR)/node_modules ## Apply every fix `lint` can make on its own
	cargo fmt --all
	@# Only what clippy marks machine-applicable, which is why this is safe to
	@# run unattended. `--allow-dirty` because a fix target is for exactly the
	@# uncommitted tree cargo would otherwise refuse to touch -- commit or
	@# stash first if you want the changes separable.
	cargo clippy --fix --all-targets --allow-dirty --allow-staged
	@if [ "$$(uname -s)" != Linux ] || pkg-config --exists webkit2gtk-4.1 2>/dev/null; then \
		cargo clippy --fix -p everyday-app --all-targets --allow-dirty --allow-staged; \
	fi
	@# Formatting a Rust file can leave it in a shape clippy reads differently
	@# and vice versa, so settle on the formatter.
	cargo fmt --all
	@# Then the two generated artefacts, in the order they depend on: the
	@# surface snapshot is written from the Rust table, and the client is
	@# generated from the snapshot.
	UPDATE_SURFACE=1 cargo test -p everyday-service --test surface
	npm --prefix $(UI_DIR) run gen:api
	@# Same order on this side: lint fixes first, formatter last.
	npm --prefix $(UI_DIR) run lint:fix
	npm --prefix $(UI_DIR) run format

fmt: ## Format Rust sources
	cargo fmt --all

cli: ## Run the everyday CLI, e.g. make cli ARGS="list"
	cargo run -p everyday-cli -- $(ARGS)

# The PNG/ICO/ICNS set beside the master is generated, not hand-drawn: edit
# icons/icon.svg and re-run this. The Tauri CLI rasterises the SVG itself
# (resvg), which is the point -- ImageMagick's built-in SVG renderer silently
# drops the gradient and hands back a black tile.
icons: ## Regenerate the app icons from crates/everyday-app/icons/icon.svg
	cd $(APP_DIR) && cargo tauri icon icons/icon.svg -o icons
	@# Desktop only: the mobile sets Tauri also emits have no shell to go with.
	rm -rf $(APP_DIR)/icons/android $(APP_DIR)/icons/ios

# Wayland hands the compositor an app id, not an icon, and GNOME resolves it
# to an icon through the .desktop file that claims that id. A binary run
# straight out of target/ has no .desktop file, so it gets the generic icon.
# Installing the .deb solves that; this is for running a build in place.
desktop-entry: ## Give a locally built binary its name and icon in the desktop
	./scripts/desktop-entry.sh

undesktop-entry: ## Undo `make desktop-entry`
	./scripts/desktop-entry.sh --remove

clean: ## Remove build output
	cargo clean
	rm -rf $(UI_DIR)/dist

distclean: clean ## Remove build output and installed npm packages
	rm -rf $(UI_DIR)/node_modules
