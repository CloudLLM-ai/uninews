# Makefile

.PHONY: build release clean fmt check test install doc help tasks

default: help

# Default target (ensures formatting before building)
build: fmt  ## Build the project in release mode (runs fmt first)
	cargo build --release

# Full release process (ensures everything runs in the correct order)
release: fmt check build test install doc  ## Perform a full release (fmt, check, build, test, install, doc)

# Format the code
fmt:  ## Format the code using cargo fmt
	cargo fmt

# Check for errors without building
check:  ## Run cargo check to analyze the code without compiling
	cargo check

# Run tests
test:  ## Run tests using cargo test
	cargo test

# Install the binary
install:  ## Install the binary to Cargo's global bin directory
	cargo install --path .

# Generate documentation
doc:  ## Generate project documentation using cargo doc
	cargo doc

# Publish to crates.io
publish: ## Publish the crate to crates.io
	cargo publish

# Clean build artifacts
clean:  ## Remove build artifacts using cargo clean
	cargo clean

# Show all available tasks
help tasks:  ## Show this help message
	@echo "Available commands:"
	@grep -E '^[a-zA-Z_-]+:.*##' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'
