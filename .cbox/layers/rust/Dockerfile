ARG BASE_IMAGE
FROM ${BASE_IMAGE}
USER root
RUN apt-get update && apt-get install -y \
    build-essential pkg-config libssl-dev curl just

# Layer-owned init.d (ADR 013). Empty for now: the .gitkeep placeholder
# keeps the directory present so cbox-init has it. No
# `chmod /cbox/init.d/*.sh` line yet — the glob would fail with no
# scripts; add it alongside the first script this layer ships.
COPY init.d/ /cbox/init.d/

USER cbox
# rustup runs as the unprivileged cbox user so the toolchain lands in
# ~/.cargo and ~/.rustup on the container's writable layer (owned by
# cbox). Pinned to `stable`; cbox's MSRV (1.85) is far below current
# stable, and there is no rust-toolchain.toml to honour yet.
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal \
        --component rustfmt,clippy

# Put cargo/rustc/clippy/rustfmt on PATH for every shell — login or not —
# so the agent's `cargo …` / `just …` commands resolve. rustup also
# appends a `. "$HOME/.cargo/env"` line to the login-shell profiles.
ENV PATH="/home/cbox/.cargo/bin:${PATH}"
