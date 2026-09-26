# The Linux environment scripts/linux-cargo and scripts/linux-run use.
FROM rust:1.96-trixie
RUN rustup component add clippy rustfmt
# A compositor to run AppKit programs under without a display (see
# scripts/headless-wayland), a screenshot tool, and fonts.
RUN apt-get update \
 && apt-get install -y --no-install-recommends sway grim fonts-dejavu-core \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home compositor
# Clipboard tools, to test copy and paste between programs under the
# headless compositor, and a session bus, to test reading desktop settings.
RUN apt-get update && apt-get install -y --no-install-recommends wl-clipboard dbus-daemon && rm -rf /var/lib/apt/lists/*
# Text: fonts for shaping, fallback and emoji tests and screenshots (Latin,
# Arabic, Hebrew and more in noto-core; CJK; color emoji).
RUN apt-get update && apt-get install -y --no-install-recommends fonts-noto-core fonts-noto-cjk fonts-noto-color-emoji && rm -rf /var/lib/apt/lists/*
# A virtual keyboard (wtype), so a program under the headless compositor
# gets keyboard focus and an input serial to set the clipboard with.
RUN apt-get update && apt-get install -y --no-install-recommends wtype && rm -rf /var/lib/apt/lists/*
