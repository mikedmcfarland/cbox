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

# Foundation image only. Useful as a quick smoke check of base/Dockerfile.
base:
    docker build -t cbox-base base/

# Build the example tier image by exercising the cbox build pipeline
# end-to-end (base -> environment -> layers -> cbox-tier-dev). This is
# the corpus the DinD integration test runs against.
example-tier:
    CBOX_CONFIG=examples/full-setup/cbox.yaml cargo run --quiet -- build dev

# Integration tests (cargo test -- --ignored). Depends on the example
# tier image so the DinD smoke test in src/backend/local_docker.rs
# can start a container via the Backend trait — no raw `docker run`.
# Forced sequential: two of these tests swap `HOME` via an RAII guard,
# and parallel runs would race the env var.
integration: example-tier
    cargo test -- --ignored --test-threads=1

build:
    cargo build --release

run *ARGS:
    cargo run -- {{ARGS}}
