FROM cbox-base
USER root
RUN apt-get update && apt-get install -y \
    zsh emacs-nox ripgrep fzf direnv locales \
    && locale-gen en_US.UTF-8
USER cbox
COPY --chown=cbox:cbox zshrc /home/cbox/.zshrc
COPY --chown=cbox:cbox gitconfig /home/cbox/.gitconfig
COPY --chown=cbox:cbox tmux.conf /home/cbox/.tmux.conf
