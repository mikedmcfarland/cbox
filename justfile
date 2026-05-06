default: check

check:
    cargo check

lint:
    cargo clippy --all-targets -- -D warnings

format:
    cargo fmt

format-check:
    cargo fmt -- --check

test:
    cargo test

base:
    docker build -t cbox-base base/

integration: base
    cargo test -- --ignored

build:
    cargo build --release

run *ARGS:
    cargo run -- {{ARGS}}
