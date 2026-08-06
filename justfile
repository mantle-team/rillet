# Rillet development commands

# List available recipes
default:
    @just --list

# Build the workspace
build:
    cargo build --workspace

# Run tests
test:
    cargo test --workspace

# Run all checks (build, lint, format, tests, features, package, semver)
check: build check-lint check-format test check-features check-package check-semver

# Run quick checks (build, lint, format)
check-quick: build check-lint check-format

# Check every feature combination
check-features:
    @echo "Checking feature combinations..."
    cargo test -p rillet --no-default-features
    cargo test -p rillet --no-default-features --features im
    cargo test -p rillet --no-default-features --features smol-str

# Check code formatting
check-format:
    @echo "Checking format..."
    cargo fmt --all -- --check

# Check clippy lints
check-lint:
    @echo "Running clippy..."
    cargo clippy --workspace --all-targets -- -D warnings

# Check tag validity
check-tag tag:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    if [ "{{tag}}" != "v$VERSION" ]; then
        echo "::error::tag {{tag}} does not match Cargo.toml version $VERSION"
        exit 1
    fi

# Publish the crates
publish:
    cargo publish -p rillet-macros
    cargo publish -p rillet

# Check crate packaging
check-package:
    @echo "Checking package..."
    cargo publish --dry-run -p rillet-macros
    cargo publish --dry-run -p rillet

# Check semver compatibility
check-semver:
    @echo "Checking semver..."
    cargo semver-checks --package rillet
