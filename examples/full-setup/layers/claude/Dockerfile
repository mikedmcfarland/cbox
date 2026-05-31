ARG BASE_IMAGE
FROM ${BASE_IMAGE}
USER root
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs \
    && npm install -g @anthropic-ai/claude-code

# Pre-create the mount point for the per-tier `.claude` named volume
# (ADR 014). Docker initializes a named volume's ownership from the
# image's mount-point dir on first mount; without this line the dir
# would be created as root:root at mount time, leaving the `cbox` user
# unable to write OAuth credentials or settings.json.
RUN mkdir -p /home/cbox/.claude && chown cbox:cbox /home/cbox/.claude

# Layer-owned init.d scripts. cbox-init runs every /cbox/init.d/*.sh on
# container start (after dockerd is up); scripts must be idempotent.
# Convention: any layer can ship its own init.d/ subdir and COPY it
# here — register MCPs, prime caches, drop config. The cbox build
# pipeline gives this directory to Docker verbatim, so $tier_image:latest
# carries all layer scripts at /cbox/init.d/.
COPY init.d/ /cbox/init.d/
RUN chmod 0755 /cbox/init.d/*.sh

USER cbox
