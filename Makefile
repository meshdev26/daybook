# Telegram Agent — one entry point for everything.
# Targets are grouped by purpose below.

SHELL := /usr/bin/env bash
VERSION := $(shell sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

## --- run & configure ------------------------------------------------------
.PHONY: setup
setup: ## Interactive first-run configuration (writes .env, 0600)
	./setup.sh

.PHONY: run
run: ## Build (debug) and run locally
	cargo run

.PHONY: tg
tg: ## Short alias: build and run Daybook locally
	@$(MAKE) run

.PHONY: run-release
run-release: ## Build (release) and run locally
	cargo build --release && ./target/release/telegram-agent

## --- quality gates --------------------------------------------------------
.PHONY: build
build: ## cargo build
	cargo build

.PHONY: test
test: ## Full gate suite: fmt, clippy, test, audit, deny
	cargo fmt --check
	cargo clippy -- -D warnings
	cargo test
	cargo audit
	cargo deny check

.PHONY: js-check
js-check: ## Dev-only: type-check + test dashboard client JS (Node required)
	npm ci --prefix assets && npm run --prefix assets check && npm test --prefix assets

.PHONY: fmt
fmt: ## Format all Rust code
	cargo fmt

.PHONY: doctor
doctor: ## Environment diagnostics (read-only)
	bash scripts/doctor.sh --fix-hints

## --- fuzzing ----------------------------------------------------------------
## Requires: rustup nightly + `cargo install cargo-fuzz`. Property tests run
## as part of `make test`; this runs a bounded libFuzzer campaign over the
## security-critical pure functions (esc, hmac+ct_eq, invoice parsing,
## gen_token).
.PHONY: fuzz
fuzz: ## Bounded fuzz campaign (FUZZ_SECS=45, sanitizer-free)
	cargo +nightly fuzz run -s none fuzz_core -- -max_total_time=$(FUZZ_SECS) -rss_limit_mb=4096

## --- distribution -----------------------------------------------------------
.PHONY: icons
icons: ## Regenerate app icons from DESIGN.md tokens
	python3 scripts/gen_icons.py

.PHONY: package
package: ## Build host-platform bundle into dist/ (.exe/.app/tar.gz)
	bash scripts/package.sh

.PHONY: install
install: ## Install onto this (existing) Linux server, systemd service
	bash scripts/install.sh

.PHONY: status
status: ## Show install status on this machine
	bash scripts/install.sh --status

.PHONY: uninstall
uninstall: ## Remove the installed service (keeps data)
	bash scripts/install.sh --uninstall

.PHONY: backup
backup: ## Back up the database safely (WAL-aware)
	bash scripts/backup.sh

.PHONY: restore
restore: ## Restore the database from a backup (args via RESTORE_ARGS='...')
	bash scripts/restore.sh $(RESTORE_ARGS)

.PHONY: dist-clean
dist-clean: ## Remove build bundles in dist/
	rm -rf dist
