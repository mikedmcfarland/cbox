ARG BASE_IMAGE=cbox-base
FROM ${BASE_IMAGE}
USER root
RUN apt-get update && apt-get install -y --no-install-recommends \
        zsh emacs-nox ripgrep fzf direnv \
    && rm -rf /var/lib/apt/lists/*
USER cbox
COPY --chown=cbox:cbox zshrc /home/cbox/.zshrc
