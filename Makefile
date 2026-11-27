# Jatayu -- one place for the commands you actually run.
#
# `make` on its own lists the targets. Nothing here reimplements the scripts
# or the toolchains; it only spares you remembering which of the four command
# styles this repo uses -- `./scripts/*.sh`, `cargo`, `cargo tauri` from the
# shell crate's directory, or `npm --prefix ui`.

# `cargo install` puts binaries in ~/.cargo/bin. rustup adds that to PATH; a
# distro-packaged cargo (/usr/bin/cargo) does not, so `cargo tauri` would not
# resolve here even with the CLI installed.
export PATH := $(HOME)/.cargo/bin:$(PATH)

APP_DIR := crates/jatayu-app
UI_DIR  := ui

# Extra arguments for the targets that forward them, e.g.
#   make test ARGS="-p jatayu-core"
#   make cli  ARGS="search rain"
ARGS ?=

.DEFAULT_GOAL := help
.PHONY: help setup app-deps run dev ui build dist test test-speech test-postgres test-imap test-smtp test-mail test-caldav test-matrix test-livekit test-call lint check fix fmt cli icons desktop-entry undesktop-entry clean distclean

help: ## Show this help
	@echo "Jatayu -- make <target>"
	@echo
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "} {printf "  \033[1m%-10s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "  Pass arguments with ARGS, e.g. make test ARGS=\"-p jatayu-core\""

setup: ## Install prerequisites: system headers, Tauri CLI, npm packages
	@if [ "$$(uname -s)" = Linux ]; then ./scripts/setup-linux.sh; fi
	@if [ "$$(uname -s)" = Darwin ]; then ./scripts/setup-macos.sh; fi
	@if command -v cargo-tauri >/dev/null 2>&1; then \
		echo "Tauri CLI: $$(cargo tauri --version)"; \
	else \
		cargo install tauri-cli --version '^2' --locked; \
	fi
	npm --prefix $(UI_DIR) install
	@node scripts/check-node.mjs

# A clean clone has no node_modules, and every target that touches the
# interface needs them. Re-runs only when the lockfile moves.
$(UI_DIR)/node_modules: $(UI_DIR)/package-lock.json
	npm --prefix $(UI_DIR) install
	@touch $@

run: $(UI_DIR)/node_modules app-deps ## Run the desktop app in development, with hot reload
	./scripts/dev.sh

dev: run ## Alias for `run`

ui: $(UI_DIR)/node_modules ## Run the interface alone in a browser, on its mock backend
	@echo "The interface falls back to an in-memory backend. Demo password: jatayu"
	npm --prefix $(UI_DIR) run dev

# Where cargo puts what it builds -- CARGO_TARGET_DIR moves it out of the
# checkout, which `desktop-entry` below looks for too.
TARGET_DIR := $(or $(CARGO_TARGET_DIR),target)

# What building the desktop shell needs on Linux beyond Rust and a C++
# compiler: the WebKitGTK headers, and -- for the llama.cpp it builds with
# Vulkan, see crates/jatayu-service/Cargo.toml -- the Vulkan headers,
# SPIRV-Headers and `glslc`, from the distribution or from a Vulkan SDK named
# by VULKAN_SDK (scripts/vulkan-sdk.sh fetches one). APP_MISSING names what
# is not here; it is empty when nothing is missing, and off Linux. `make
# setup` installs both.
ifeq ($(shell uname -s),Linux)
HAVE_WEBKIT := $(shell pkg-config --exists webkit2gtk-4.1 2>/dev/null && echo yes)
HAVE_VULKAN := $(or $(VULKAN_SDK),$(shell command -v glslc >/dev/null \
	&& [ -f /usr/include/vulkan/vulkan.hpp ] \
	&& [ -f /usr/include/spirv/unified1/spirv.hpp ] && echo yes))
APP_MISSING := $(strip $(if $(HAVE_WEBKIT),,WebKitGTK) $(if $(HAVE_VULKAN),,Vulkan))
endif
# Printed in single quotes, so the command in it is shown, not run.
VULKAN_HINT := or set VULKAN_SDK to a Vulkan SDK: export VULKAN_SDK=$$(scripts/vulkan-sdk.sh ~/.cache/vulkan-sdk)

# Stops a build that would otherwise fail minutes in, at a pkg-config or
# CMake error, and says what to install instead.
app-deps:
	@if [ -n "$(APP_MISSING)" ]; then \
		echo "Building the app needs the $(APP_MISSING) development files: run make setup" >&2; \
		$(if $(findstring Vulkan,$(APP_MISSING)),echo '$(VULKAN_HINT)' >&2;) \
		exit 1; \
	fi

# Two builds, for two different people. `build` is the app for this machine,
# run in place (`make desktop-entry` points the desktop at it): optimised
# under the `release` profile, which is tuned to build quickly, and no
# installers, which take longer to pack than the app takes to rebuild and
# which nobody running it in place opens. `dist` is what a release ships --
# the `dist` profile's LTO and every installer this platform makes, the
# same command `.github/workflows/release.yml` runs.
#
# Both run more compilers at once than .cargo/config.toml's four. That cap
# is for the builds that run under a memory ceiling (`run`, `test`); these
# do not, and most of a rebuild is a chain of three crates -- core, service,
# the app -- each of whose sixteen codegen units waits on a free job. With
# eight, a rebuild after a change to jatayu-core took 160 s rather than
# 223 s, and peaked at 5.6 GB, under what four jobs had needed under the
# old profile. `make build JOBS=4` on a machine with less to spare.
JOBS ?= 8

build: $(UI_DIR)/node_modules app-deps ## Build the desktop app to run on this machine
	cd $(APP_DIR) && CARGO_BUILD_JOBS=$(JOBS) cargo tauri build --no-bundle -- --locked
	@echo
	@echo "Built $(TARGET_DIR)/release/jatayu-app"

dist: $(UI_DIR)/node_modules app-deps ## Build the installers the way a release does (slow)
	cd $(APP_DIR) && CARGO_BUILD_JOBS=$(JOBS) cargo tauri build -- --profile dist --locked
	@echo
	@echo "Installers are under $(TARGET_DIR)/dist/bundle/"

test: ## Run the test suite under a memory cap
	./scripts/test.sh $(ARGS)
	@# The shell is not a workspace default member -- it links the platform
	@# webview -- so a plain `cargo test` skips it, and its tests would
	@# otherwise run nowhere at all. That includes the one holding a feed's
	@# subscription URL, which is a bearer credential, out of error messages
	@# and logs. Skipped when ARGS names a package, and when what it takes to
	@# build it is missing (APP_MISSING); CI installs it and runs it
	@# unconditionally.
	@if [ -z "$(ARGS)" ] && [ -z "$(APP_MISSING)" ]; then \
		./scripts/test.sh -p jatayu-app; \
	fi
	@# Not the `speech` feature -- see `test-speech` below for why that is a
	@# separate, opt-in target rather than folded in here.

# The storage conformance suite against a real Postgres, which `make test`
# cannot do because it would need a server it has no business starting. CI
# runs the same suite against a service container on every push; this is the
# way to run it before pushing. The container is removed when it stops.
test-postgres: ## Run the storage suite against a throwaway Postgres in Docker
	@docker rm -f jatayu-pgtest >/dev/null 2>&1 || true
	docker run -d --rm --name jatayu-pgtest \
		-e POSTGRES_PASSWORD=test -e POSTGRES_DB=jatayu_test \
		-p 55432:5432 postgres:16-alpine
	@echo "waiting for Postgres..."
	@for i in $$(seq 1 60); do \
		docker exec jatayu-pgtest pg_isready -q && break; \
		sleep 1; \
	done
	@JATAYU_TEST_DATABASE_URL=postgresql://postgres:test@127.0.0.1:55432/jatayu_test \
		cargo test -p jatayu-store-postgres; \
	status=$$?; \
	docker rm -f jatayu-pgtest >/dev/null; \
	exit $$status

# The IMAP adapter's suite (crates/jatayu-mail/tests/imap_dovecot.rs),
# against a throwaway Dovecot -- the official image, confined and rootless,
# which authenticates any username against the one password in
# USER_PASSWORD and auto-creates that user's mailbox, so nothing here seeds
# a user database. It supports CONDSTORE, QRESYNC, IDLE, MOVE, UIDPLUS and
# SPECIAL-USE without extra configuration. `insecure-test-tls` is the
# feature that makes this crate trust the container's self-signed
# certificate; see `Security::InsecureTestTls` in `src/imap.rs` for why it
# exists and why it is never on by default. The container is removed when
# it stops.
test-imap: ## Run the IMAP adapter's suite against a throwaway Dovecot in Docker
	@docker rm -f jatayu-imaptest >/dev/null 2>&1 || true
	docker run -d --rm --name jatayu-imaptest \
		-e USER_PASSWORD=testpass \
		-p 15993:31993 -p 15143:31143 \
		dovecot/dovecot:latest >/dev/null
	@echo "waiting for Dovecot..."
	@bash -c 'for i in $$(seq 1 60); do \
		(exec 3<>/dev/tcp/127.0.0.1/15993) 2>/dev/null && exec 3<&- 3>&- && exit 0; \
		sleep 1; \
	done; exit 1'
	@JATAYU_TEST_IMAP=1 \
		JATAYU_TEST_IMAP_HOST=127.0.0.1 \
		JATAYU_TEST_IMAP_TLS_PORT=15993 \
		JATAYU_TEST_IMAP_STARTTLS_PORT=15143 \
		JATAYU_TEST_IMAP_USER=jatayu \
		JATAYU_TEST_IMAP_PASS=testpass \
		cargo test -p jatayu-mail --features insecure-test-tls --test imap_dovecot; \
	status=$$?; \
	docker rm -f jatayu-imaptest >/dev/null; \
	exit $$status

# The SMTP client's suite (crates/jatayu-mail/tests/smtp_mailpit.rs),
# against a throwaway Mailpit -- the official image, which is given exactly
# one SMTP credential (MP_SMTP_AUTH=jatayu:testpass) so the suite can
# prove both that the right password is accepted and that a wrong one comes
# back as this crate's own auth error, and exposes an HTTP API the test
# reads delivered messages back through, so nothing here needs its own
# IMAP-style server to check what arrived. Auth is exercised in the clear
# (MP_SMTP_AUTH_ALLOW_INSECURE): this suite is about the SMTP verb sequence
# and this crate's own error mapping, not about proving
# `rustls-platform-verifier` again -- `make test-imap` already does that
# against Dovecot's TLS, and `smtp.rs`'s `connect` uses the identical
# `tokio1-rustls` configuration. The container is removed when it stops.
test-smtp: ## Run the SMTP client's suite against a throwaway Mailpit in Docker
	@docker rm -f jatayu-smtptest >/dev/null 2>&1 || true
	docker run -d --rm --name jatayu-smtptest \
		-e MP_SMTP_AUTH=jatayu:testpass -e MP_SMTP_AUTH_ALLOW_INSECURE=1 \
		-p 11025:1025 -p 18025:8025 \
		axllent/mailpit:latest >/dev/null
	@echo "waiting for Mailpit..."
	@bash -c 'for i in $$(seq 1 60); do \
		(exec 3<>/dev/tcp/127.0.0.1/18025) 2>/dev/null && exec 3<&- 3>&- && exit 0; \
		sleep 1; \
	done; exit 1'
	@JATAYU_TEST_SMTP=1 \
		JATAYU_TEST_SMTP_HOST=127.0.0.1 \
		JATAYU_TEST_SMTP_PORT=11025 \
		JATAYU_TEST_SMTP_API=http://127.0.0.1:18025 \
		cargo test -p jatayu-mail --features insecure-test-tls --test smtp_mailpit; \
	status=$$?; \
	docker rm -f jatayu-smtptest >/dev/null; \
	exit $$status

# `accountcal::caldav`'s suite (crates/jatayu-service/tests/caldav_docker.rs),
# against a throwaway Radicale -- Kozea's own image, GPL-3.0 licensed, which
# is fine for a container this application never links: nothing here ships
# Radicale, it only talks CalDAV to a copy of it that is deleted when the
# test ends. Authenticated with a plain htpasswd file (one line, written to a
# temp file below and never committed) rather than `--auth-type none`,
# because Basic auth over a real server is exactly the credential path
# `accountcal::caldav` uses for iCloud, Fastmail and Custom accounts, and a
# test that skipped it would not be testing that path at all. The test
# itself seeds the calendar and its events over plain HTTP PUT -- see its own
# doc for why -- so nothing here does more than start the server and wait for
# it to answer. The container is removed when it stops.
test-caldav: ## Run the CalDAV adapter's suite against a throwaway Radicale in Docker
	@docker rm -f jatayu-caldavtest >/dev/null 2>&1 || true
	@tmp_htpasswd=$$(mktemp); \
	printf 'jatayu:testpass\n' > $$tmp_htpasswd; \
	docker run -d --rm --name jatayu-caldavtest \
		-v $$tmp_htpasswd:/etc/radicale/users:ro \
		-p 15232:5232 \
		kozea/radicale:latest --hosts 0.0.0.0:5232 \
		--auth-type htpasswd --auth-htpasswd-filename /etc/radicale/users \
		--auth-htpasswd-encryption plain >/dev/null; \
	echo "waiting for Radicale..."; \
	for i in $$(seq 1 60); do \
		curl -sf -o /dev/null http://127.0.0.1:15232/ && break; \
		sleep 1; \
	done; \
	JATAYU_TEST_CALDAV=1 \
		JATAYU_TEST_CALDAV_URL=http://127.0.0.1:15232 \
		JATAYU_TEST_CALDAV_USER=jatayu \
		JATAYU_TEST_CALDAV_PASS=testpass \
		cargo test -p jatayu-service --test caldav_docker; \
	status=$$?; \
	docker rm -f jatayu-caldavtest >/dev/null; \
	rm -f $$tmp_htpasswd; \
	exit $$status

test-matrix: ## Run the chat bot end to end against a throwaway Conduit in Docker
	@docker rm -f jatayu-matrixtest >/dev/null 2>&1 || true
	@docker run -d --rm --name jatayu-matrixtest \
		-p 127.0.0.1:16167:6167 \
		-e CONDUIT_SERVER_NAME=chat-test.localhost \
		-e CONDUIT_DATABASE_PATH=/var/lib/matrix-conduit/ \
		-e CONDUIT_DATABASE_BACKEND=rocksdb \
		-e CONDUIT_PORT=6167 -e CONDUIT_ADDRESS=0.0.0.0 \
		-e CONDUIT_ALLOW_REGISTRATION=true -e CONDUIT_ALLOW_FEDERATION=false \
		-e CONDUIT_ALLOW_CHECK_FOR_UPDATES=false -e CONDUIT_TRUSTED_SERVERS='[]' \
		-e CONDUIT_CONFIG='' \
		--tmpfs /var/lib/matrix-conduit:rw,size=512m \
		matrixconduit/matrix-conduit:v0.10.12 >/dev/null; \
	echo "waiting for Conduit..."; \
	for i in $$(seq 1 60); do \
		curl -sf -o /dev/null http://127.0.0.1:16167/_matrix/client/versions && break; \
		sleep 1; \
	done; \
	JATAYU_TEST_MATRIX_URL=http://127.0.0.1:16167 \
		cargo test -p jatayu-chat --test matrix_live -- --nocapture; \
	status=$$?; \
	docker rm -f jatayu-matrixtest >/dev/null; \
	exit $$status

# A call's media against a real LiveKit server (crates/jatayu-chat/tests/
# sfu_live.rs). Host networking, on loopback and ports of its own, so the
# media's ICE candidates are 127.0.0.1 and nothing is published; a throwaway
# API key signs the test's tokens.
test-livekit: ## Run a call's media end to end against a throwaway LiveKit server in Docker
	@docker rm -f jatayu-livekittest >/dev/null 2>&1 || true
	@docker run -d --rm --name jatayu-livekittest --network host \
		-e LIVEKIT_KEYS="testkey: throwaway-secret-for-tests-only-0123456789" -e NODE_IP=127.0.0.1 \
		-e LIVEKIT_CONFIG="$$(printf 'port: 17880\nbind_addresses: [127.0.0.1]\nrtc:\n  tcp_port: 17881\n  udp_port: 17882\n  use_external_ip: false\n')" \
		livekit/livekit-server:v1.13.11 >/dev/null; \
	echo "waiting for LiveKit..."; \
	for i in $$(seq 1 30); do \
		curl -sf -o /dev/null http://127.0.0.1:17880/ && break; \
		sleep 1; \
	done; \
	JATAYU_TEST_LIVEKIT_URL=ws://127.0.0.1:17880 \
	JATAYU_TEST_LIVEKIT_KEY=testkey \
	JATAYU_TEST_LIVEKIT_SECRET=throwaway-secret-for-tests-only-0123456789 \
		cargo test -p jatayu-chat --test sfu_live -- --ignored --nocapture $(ARGS); \
	status=$$?; \
	docker logs jatayu-livekittest > target/livekit-test.log 2>&1 || true; \
	docker rm -f jatayu-livekittest >/dev/null; \
	exit $$status

# A call to the assistant end to end (crates/jatayu-chat/tests/call_live.rs):
# the real runner and speech models answering a MatrixRTC call. Four
# throwaway servers, all on loopback with host networking: Conduit with
# federation on, nginx in front of it with a self-signed certificate (the
# token service checks OpenID tokens over the federation API, which is HTTPS
# only), LiveKit, and lk-jwt-service. The server name carries the proxy's
# port, so federation goes straight to it. Speech models come from
# MODELS_DIR, linked into a scratch data folder: by default this user's own
# installation's, under the newest name it has -- Jatayu's, or a former
# name's (Karma Yoga, Every Day) until a Jatayu build has carried it across.
CALLTEST := target/calltest
MODELS_DIR ?= $(or $(firstword $(wildcard $(foreach name,jatayu karmayoga everyday,$(HOME)/.local/share/$(name)/models))),$(HOME)/.local/share/jatayu/models)
test-call: ## Run a call to the assistant end to end: throwaway Conduit, LiveKit and token service, real speech
	@docker rm -f jatayu-calltest-conduit jatayu-calltest-tls jatayu-calltest-livekit jatayu-calltest-jwt >/dev/null 2>&1 || true
	@mkdir -p $(CALLTEST)/data/jatayu/models
	@for m in parakeetV3 speechKit phonon; do ln -sfn $(MODELS_DIR)/$$m $(CALLTEST)/data/jatayu/models/$$m; done
	@openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj /CN=rtc-test.localhost \
		-keyout $(CALLTEST)/key.pem -out $(CALLTEST)/cert.pem 2>/dev/null
	@printf 'events {}\nhttp { server { listen 127.0.0.1:18448 ssl; ssl_certificate /tls/cert.pem; ssl_certificate_key /tls/key.pem;\n  location / { proxy_pass http://127.0.0.1:16168; proxy_set_header Host $$host; } } }\n' > $(CALLTEST)/nginx.conf
	@docker run -d --rm --name jatayu-calltest-conduit --network host \
		-e CONDUIT_SERVER_NAME=rtc-test.localhost:18448 \
		-e CONDUIT_DATABASE_PATH=/var/lib/matrix-conduit/ -e CONDUIT_DATABASE_BACKEND=rocksdb \
		-e CONDUIT_PORT=16168 -e CONDUIT_ADDRESS=127.0.0.1 \
		-e CONDUIT_ALLOW_REGISTRATION=true -e CONDUIT_ALLOW_FEDERATION=true \
		-e CONDUIT_ALLOW_CHECK_FOR_UPDATES=false -e CONDUIT_TRUSTED_SERVERS='[]' -e CONDUIT_CONFIG='' \
		--tmpfs /var/lib/matrix-conduit:rw,size=512m \
		matrixconduit/matrix-conduit:v0.10.12 >/dev/null
	@docker run -d --rm --name jatayu-calltest-tls --network host \
		-v $(CURDIR)/$(CALLTEST)/nginx.conf:/etc/nginx/nginx.conf:ro -v $(CURDIR)/$(CALLTEST):/tls:ro \
		nginx:alpine >/dev/null
	@docker run -d --rm --name jatayu-calltest-livekit --network host \
		-e LIVEKIT_KEYS="testkey: throwaway-secret-for-tests-only-0123456789" -e NODE_IP=127.0.0.1 \
		-e LIVEKIT_CONFIG="$$(printf 'port: 17880\nbind_addresses: [127.0.0.1]\nrtc:\n  tcp_port: 17881\n  udp_port: 17882\n  use_external_ip: false\n')" \
		livekit/livekit-server:v1.13.11 >/dev/null
	@docker run -d --rm --name jatayu-calltest-jwt --network host \
		--add-host rtc-test.localhost:127.0.0.1 \
		-e LIVEKIT_JWT_BIND=127.0.0.1:18080 -e LIVEKIT_URL=ws://127.0.0.1:17880 \
		-e LIVEKIT_KEY=testkey -e LIVEKIT_SECRET=throwaway-secret-for-tests-only-0123456789 \
		-e LIVEKIT_FULL_ACCESS_HOMESERVERS=rtc-test.localhost:18448 \
		-e LIVEKIT_INSECURE_SKIP_VERIFY_TLS=YES_I_KNOW_WHAT_I_AM_DOING \
		ghcr.io/element-hq/lk-jwt-service:0.10.0 >/dev/null; \
	echo "waiting for Conduit, LiveKit and the token service..."; \
	for i in $$(seq 1 60); do \
		curl -sf -o /dev/null http://127.0.0.1:16168/_matrix/client/versions \
			&& curl -sf -o /dev/null http://127.0.0.1:17880/ \
			&& curl -sf -o /dev/null http://127.0.0.1:18080/healthz && break; \
		sleep 1; \
	done; \
	XDG_DATA_HOME=$(CURDIR)/$(CALLTEST)/data JATAYU_TEST_SPEECH=1 \
	JATAYU_TEST_MATRIX_URL=http://127.0.0.1:16168 \
	JATAYU_TEST_LIVEKIT_JWT_URL=http://127.0.0.1:18080 \
		cargo test -p jatayu-chat --features speech --test call_live -- --ignored --nocapture $(ARGS); \
	status=$$?; \
	for c in conduit tls livekit jwt; do docker logs jatayu-calltest-$$c > $(CALLTEST)/$$c.log 2>&1 || true; done; \
	docker rm -f jatayu-calltest-conduit jatayu-calltest-tls jatayu-calltest-livekit jatayu-calltest-jwt >/dev/null; \
	exit $$status

# `jatayu-service`'s own end-to-end suite
# (crates/jatayu-service/tests/mailsync_dovecot.rs) needs both servers at
# once -- a real IMAP account to sync, act on and read back from, and a real
# SMTP relay to send through -- so this starts both throwaway containers
# together rather than asking for two separate `make` invocations. Same
# images, same credentials, same ports as `test-imap` and `test-smtp` above;
# both are removed when either half of the suite finishes.
test-mail: ## Run the mail sync engine's end-to-end suite against throwaway Dovecot and Mailpit
	@docker rm -f jatayu-imaptest jatayu-smtptest >/dev/null 2>&1 || true
	docker run -d --rm --name jatayu-imaptest \
		-e USER_PASSWORD=testpass \
		-p 15993:31993 -p 15143:31143 \
		dovecot/dovecot:latest >/dev/null
	docker run -d --rm --name jatayu-smtptest \
		-e MP_SMTP_AUTH=jatayu:testpass -e MP_SMTP_AUTH_ALLOW_INSECURE=1 \
		-p 11025:1025 -p 18025:8025 \
		axllent/mailpit:latest >/dev/null
	@echo "waiting for Dovecot and Mailpit..."
	@bash -c 'for i in $$(seq 1 60); do \
		(exec 3<>/dev/tcp/127.0.0.1/15993) 2>/dev/null && exec 3<&- 3>&- && exit 0; \
		sleep 1; \
	done; exit 1'
	@bash -c 'for i in $$(seq 1 60); do \
		(exec 3<>/dev/tcp/127.0.0.1/18025) 2>/dev/null && exec 3<&- 3>&- && exit 0; \
		sleep 1; \
	done; exit 1'
	@JATAYU_TEST_IMAP=1 \
		JATAYU_TEST_IMAP_HOST=127.0.0.1 \
		JATAYU_TEST_IMAP_TLS_PORT=15993 \
		JATAYU_TEST_IMAP_STARTTLS_PORT=15143 \
		JATAYU_TEST_IMAP_USER=jatayu \
		JATAYU_TEST_IMAP_PASS=testpass \
		JATAYU_TEST_SMTP=1 \
		JATAYU_TEST_SMTP_HOST=127.0.0.1 \
		JATAYU_TEST_SMTP_PORT=11025 \
		JATAYU_TEST_SMTP_API=http://127.0.0.1:18025 \
		cargo test -p jatayu-service --test mailsync_dovecot; \
	status=$$?; \
	docker rm -f jatayu-imaptest jatayu-smtptest >/dev/null; \
	exit $$status

# The `speech` feature (`local::files`, `meeting::speech`,
# `meeting::transcribe::local`; see docs/plans/meeting-notes.md) links
# sherpa-onnx, whose build script downloads a ~20 MB prebuilt native library
# the first time it builds, unless `SHERPA_ONNX_LIB_DIR` or
# `SHERPA_ONNX_ARCHIVE_DIR` is already set -- see
# `crates/jatayu-service/Cargo.toml`'s own comment on that dependency.
# The `decisions` feature (`local::decide`; see docs/plans/mail-sorting.md)
# compiles llama.cpp from source, which needs CMake (point `CMAKE` at one
# that is not on PATH) and a C++ compiler -- and on Linux, where it is built
# with Vulkan, the Vulkan headers and `glslc` too (HAVE_VULKAN above).
# Neither `test` nor `lint` below build them, on purpose: a plain clone's
# first `make test` must not need that download or a native toolchain it may
# not have. This is the manual equivalent of .github/workflows/check.yml's
# separate `rust-speech` job, and the one job of the three there that `lint`
# does not otherwise mirror.
test-speech: ## Clippy and test the local-model features (downloads sherpa-onnx, builds llama.cpp)
	@if [ "$$(uname -s)" = Linux ] && [ -z "$(HAVE_VULKAN)" ]; then \
		echo "llama.cpp is built with Vulkan here: run make setup," >&2; \
		echo '$(VULKAN_HINT)' >&2; \
		exit 1; \
	fi
	cargo clippy -p jatayu-service --all-targets --features speech,decisions -- -D warnings
	cargo test -p jatayu-service --features speech,decisions

# The pair to reach for: `lint` says what is wrong, `fix` fixes what it can.
# They are the same tools in the same order, so anything `fix` silences is
# something `lint` would have complained about, and what survives a `fix` is
# the list of things that need a person.
#
# Between them, the `rust` and `ui` jobs in .github/workflows/check.yml run
# exactly what `lint` runs -- split in two only so the Rust half and the
# interface half go in parallel. `check.yml`'s third job, `rust-speech`, is
# not one of the two: `make test-speech` above is its manual equivalent,
# kept out of `lint` for the same reason it is kept out of `test`. Keep
# `lint` and the two jobs it does mirror in step when you change either: a
# check you can only discover from a red build ten minutes after pushing is
# one people learn to ignore.

lint: $(UI_DIR)/node_modules ## Format check, clippy and interface typecheck -- changes nothing
	cargo fmt --all -- --check
	cargo clippy --all-targets -- -D warnings
	@# The shell links the platform webview, so it is not a workspace default
	@# member and cannot be linted on a machine without those headers, or
	@# the Vulkan ones (APP_MISSING).
	@if [ -z "$(APP_MISSING)" ]; then \
		cargo clippy -p jatayu-app --all-targets -- -D warnings; \
	else \
		echo "skipping jatayu-app: $(APP_MISSING) headers missing, run make setup"; \
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
	@if [ -z "$(APP_MISSING)" ]; then \
		cargo clippy --fix -p jatayu-app --all-targets --allow-dirty --allow-staged; \
	fi
	@# Formatting a Rust file can leave it in a shape clippy reads differently
	@# and vice versa, so settle on the formatter.
	cargo fmt --all
	@# Then the two generated artefacts, in the order they depend on: the
	@# surface snapshot is written from the Rust table, and the client is
	@# generated from the snapshot.
	UPDATE_SURFACE=1 cargo test -p jatayu-service --test surface
	npm --prefix $(UI_DIR) run gen:api
	@# Same order on this side: lint fixes first, formatter last.
	npm --prefix $(UI_DIR) run lint:fix
	npm --prefix $(UI_DIR) run format

fmt: ## Format Rust sources
	cargo fmt --all

cli: ## Run the jatayu CLI, e.g. make cli ARGS="list"
	cargo run -p jatayu-cli -- $(ARGS)

# The PNG/ICO/ICNS set beside the masters is generated, not hand-drawn: edit
# the SVGs in icons/ (icon.svg and its siblings for iOS and Android) and
# re-run this. The Tauri CLI rasterises the SVG itself
# (resvg), which is the point -- ImageMagick's built-in SVG renderer silently
# drops the gradient and hands back a black tile.
icons: ## Regenerate every icon from the masters in crates/jatayu-app/icons
	@# One run over icons.json makes the desktop set (PNGs, .icns, .ico, the
	@# Windows tiles) from icon.svg, and Android's adaptive icon from its three
	@# layers, icon-android-{background,foreground,monochrome}.svg. Then the
	@# browser extension's icons and the web version's, from icon.svg again.
	@# The web's favicon.ico, apple-touch-icon.png and icon-maskable-512.png
	@# are not remade here; docs/mobile.md says how they were made.
	t=$$(mktemp -d) && trap 'rm -rf "$$t"' EXIT && root=$$(pwd) && cd $(APP_DIR) && \
	cargo tauri icon icons/icons.json -o $$t/app >/dev/null && \
	cp $$t/app/*.png $$t/app/icon.icns $$t/app/icon.ico icons/ && \
	cp -r $$t/app/android/* gen/android/app/src/main/res/ && \
	cargo tauri icon icons/icon.svg -o $$t/png --png 16,32,48,128,192,512 >/dev/null && \
	for s in 16 32 48 128; do cp $$t/png/$${s}x$${s}.png $$root/extensions/browser/icons/icon-$$s.png; done && \
	cp $$t/png/192x192.png $$root/$(UI_DIR)/public/icon-192.png && \
	cp $$t/png/512x512.png $$root/$(UI_DIR)/public/icon-512.png
	@# iOS draws its own rounded corners over a square, so its set comes from
	@# icon-ios.svg -- the tile alone, full bleed -- flattened onto the tile's
	@# midnight: the App Store refuses an icon with alpha, and `--ios-color`
	@# leaves it in. On a Mac only (the flattening is Swift). Nothing is
	@# copied unless every icon was flattened, and the scratch folder goes
	@# either way.
	@if [ "$$(uname -s)" = Darwin ]; then \
		t=$$(mktemp -d) && trap 'rm -rf "$$t"' EXIT && \
		(cd $(APP_DIR) && cargo tauri icon icons/icon-ios.svg -o $$t/out >/dev/null) && \
		swift scripts/flatten-png.swift '#161a4d' $$t/out/ios/*.png && \
		cp $$t/out/ios/*.png $(APP_DIR)/gen/apple/Assets.xcassets/AppIcon.appiconset/; \
	else \
		echo "iOS icons not regenerated: that needs a Mac (scripts/flatten-png.swift)"; \
	fi

# Wayland hands the compositor an app id, not an icon, and GNOME resolves it
# to an icon through the .desktop file that claims that id. A binary run
# straight out of target/ has no .desktop file, so it gets the generic icon.
# Installing the .deb solves that; this is for running a build in place.
# It looks under CARGO_TARGET_DIR when set; ARGS names a binary explicitly.
# The entry points at that one build, so re-run it when you switch builds.
desktop-entry: ## Give a locally built binary its name and icon in the desktop
	./scripts/desktop-entry.sh $(ARGS)

undesktop-entry: ## Undo `make desktop-entry`
	./scripts/desktop-entry.sh --remove

clean: ## Remove build output
	cargo clean
	rm -rf $(UI_DIR)/dist

distclean: clean ## Remove build output and installed npm packages
	rm -rf $(UI_DIR)/node_modules
