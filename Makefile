# ccstack — build & dev tasks.
# Build output goes to ./build (see .cargo/config.toml). Run `make` or `make help`.

BUILD_DIR ?= build
BIN       := $(BUILD_DIR)/release/ccstack
PREFIX    ?= $(HOME)/.local
ARGS      ?=

# All cargo invocations from this Makefile build into ./build (gitignored).
export CARGO_TARGET_DIR := $(BUILD_DIR)

.DEFAULT_GOAL := help
.PHONY: help build release run check test fmt fmt-check lint clippy ci install uninstall clean

help: ## List available targets
	@awk -F':.*## ' '/^[a-zA-Z_-]+:.*## /{printf "  %-12s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Debug build
	cargo build

release: ## Optimized release build -> $(BIN)
	cargo build --release

run: ## Run locally; pass args with ARGS="stats --json"
	cargo run -- $(ARGS)

check: ## Type-check only (no binary)
	cargo check

test: ## Run tests
	cargo test

fmt: ## Format sources
	cargo fmt

fmt-check: ## Verify formatting (CI)
	cargo fmt --check

clippy: ## Lint (warnings = errors)
	cargo clippy --all-targets -- -D warnings

lint: clippy ## Alias for clippy

ci: fmt-check clippy test release ## Full local CI gate

install: release ## Install ccstack to $(PREFIX)/bin
	install -d $(PREFIX)/bin
	install -m 0755 $(BIN) $(PREFIX)/bin/ccstack
	@echo "installed -> $(PREFIX)/bin/ccstack"

uninstall: ## Remove the installed binary
	rm -f $(PREFIX)/bin/ccstack

clean: ## Remove build artifacts ($(BUILD_DIR))
	cargo clean
