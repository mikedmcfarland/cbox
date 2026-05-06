# cbox-base: foundation image for every cbox tier.
#
# Ships DinD, sshd, dtach, tmux, bubblewrap, plus the entrypoint and
# supervisord that start dockerd and sshd before exec'ing the tier
# command. Layers (`environment`, then per-language) build FROM this.
#
# Container must run --privileged for DinD + bubblewrap full strength.

FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive

# System packages. docker.io + containerd give us inner dockerd; the rest
# are the runtime tools every tier needs (sshd for ADR 004, dtach for
# interactive sessions per ADR 005, tmux for autonomous sessions,
# bubblewrap for the Claude sandbox per the security model section).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        jq \
        socat \
        sudo \
        tmux \
        dtach \
        bubblewrap \
        openssh-server \
        supervisor \
        docker.io \
        containerd \
        iproute2 \
        iptables \
        uidmap \
        locales \
        tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && locale-gen en_US.UTF-8

ENV LANG=en_US.UTF-8 \
    LC_ALL=en_US.UTF-8

# Non-root user that owns interactive sessions. Passwordless sudo so init
# scripts and tier setup can adjust the container without re-baking.
RUN useradd --create-home --shell /bin/bash --uid 1000 cbox \
    && usermod -aG docker cbox \
    && echo 'cbox ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/cbox \
    && chmod 0440 /etc/sudoers.d/cbox

# cbox runtime layout:
#   /cbox/init.d/      one-shot scripts run after dockerd is up
#   /run/cbox/         session sockets (dtach + tmux); see plan §Session tracking
#   /workspace/        per-session worktrees
RUN mkdir -p /cbox/init.d /run/cbox /workspace \
    && chown -R cbox:cbox /run/cbox /workspace \
    && chmod 0755 /cbox /cbox/init.d \
    && chmod 0775 /run/cbox

# sshd: host keys generated on first start (entrypoint); config disables
# passwords. Authorized keys are injected at runtime — see ADR 012.
RUN mkdir -p /var/run/sshd /home/cbox/.ssh \
    && chown cbox:cbox /home/cbox/.ssh \
    && chmod 0700 /home/cbox/.ssh

COPY sshd_config /etc/ssh/sshd_config
COPY daemon.json /etc/docker/daemon.json
COPY supervisord.conf /etc/supervisor/conf.d/cbox.conf
COPY entrypoint.sh /usr/local/bin/cbox-entrypoint

RUN chmod 0755 /usr/local/bin/cbox-entrypoint

EXPOSE 22

ENTRYPOINT ["/usr/local/bin/cbox-entrypoint"]
CMD ["/usr/bin/supervisord", "-c", "/etc/supervisor/supervisord.conf"]
