# Makefile
.PHONY: build release clean fmt check test install doc help tasks clippy publish

default: help
CARGO_CMD=/usr/bin/env cargo
# cargo-sweep (cargo install cargo-sweep), when installed, deletes whatever an older build
# left in target/ that this build didn't use, so only fresh release artifacts remain.
SWEEP := $(shell command -v cargo-sweep >/dev/null 2>&1 && echo cargo sweep)

.PHONY: build release clean fmt check test install doc help tasks

default: help

# Default target (ensures formatting before building)
build: fmt  ## Build the project in release mode (runs fmt first), then sweep stale artifacts
	$(if $(SWEEP),$(SWEEP) --stamp)
	${CARGO_CMD} build --release
	$(if $(SWEEP),$(SWEEP) --file)

# Full release process (ensures everything runs in the correct order)
release: fmt check build test install doc  ## Perform a full release (fmt, check, build, test, install, doc)

# Format the code
fmt:  ## Format the code using cargo fmt
	${CARGO_CMD} fmt

# Check for errors without building
check:  ## Run cargo check --release to analyze the code without compiling
	${CARGO_CMD} check --release

# Strict linter, fails on warning and suggests fixes
clippy: ## Checks a package to catch common mistakes and improve your Rust code
	${CARGO_CMD} fmt
	${CARGO_CMD} clippy --release --package uninews --lib
	${CARGO_CMD} clippy --release -- -D warnings

# Run tests
test:  ## Run tests using cargo test --release
	${CARGO_CMD} test --release

# Install the binary
install:  ## Install the binary to Cargo's global bin directory
	${CARGO_CMD} install --path .

# Generate documentation
doc:  ## Generate project documentation using cargo doc --release
	${CARGO_CMD} doc --release

# Publish to crates.io
publish: ## Publish the crate to crates.io
	${CARGO_CMD} publish

# Clean build artifacts
clean:  ## Remove build artifacts using cargo clean
	${CARGO_CMD} clean

# Show all available tasks
help tasks:  ## Show this help message
	@echo "Available commands:"
	@grep -E '^[a-zA-Z_-]+:.*##' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'
