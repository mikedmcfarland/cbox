ARG BASE_IMAGE
FROM ${BASE_IMAGE}
USER root
RUN apt-get update && apt-get install -y \
    python3 python3-venv python3-pip pipx \
    && pipx install poetry uv
USER cbox
