# The Linux environment scripts/linux-cargo and scripts/linux-run use.
FROM rust:1.96-trixie
RUN rustup component add clippy rustfmt
# A compositor to run AppKit programs under without a display (see
# scripts/headless-wayland), a screenshot tool, and fonts.
RUN apt-get update \
 && apt-get install -y --no-install-recommends sway grim fonts-dejavu-core \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home compositor
