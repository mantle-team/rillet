# Rillet development commands

# List available recipes
default:
    @just --list

# Build the library
build:
    cargo build

# Run tests
test:
    cargo test

# Run all checks (build, lint, format, tests, package, semver)
check: build check-lint check-format test check-package check-semver

# Run quick checks (build, lint, format)
check-quick: build check-lint check-format

# Check code formatting
check-format:
    @echo "Checking format..."
    cargo fmt -- --check

# Check clippy lints
check-lint:
    @echo "Running clippy..."
    cargo clippy -- -D warnings

# Check tag validity
check-tag tag:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    if [ "{{tag}}" != "v$VERSION" ]; then
        echo "::error::tag {{tag}} does not match Cargo.toml version $VERSION"
        exit 1
    fi

# Publish the crate
publish:
    cargo publish

# Check crate packaging
check-package:
    @echo "Checking package..."
    cargo publish --dry-run

# Check semver compatibility
check-semver:
    @echo "Checking semver..."
    cargo semver-checks
