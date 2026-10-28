# The Linux environment scripts/linux-cargo and scripts/linux-run use.
FROM rust:1.96-trixie AS sway-build
# Trixie's Sway 1.10.1 leaves layer surfaces alive when their output goes.
# Backport the upstream fix (released in 1.11) so the desktop test can
# require the closed event, as the layer-shell protocol does.
WORKDIR /build
RUN sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/debian.sources \
 && apt-get update \
 && apt-get build-dep -y --no-install-recommends sway \
 && apt-get source sway=1.10.1-2
COPY sway-close-layer-on-output-destroy.patch /build/
RUN cd sway-1.10.1 \
 && patch -p1 < ../sway-close-layer-on-output-destroy.patch \
 && meson setup build --buildtype=release --prefix=/usr --sysconfdir=/etc \
      -Dman-pages=disabled -Dswaybar=false -Dswaynag=false -Dtray=disabled -Dgdk-pixbuf=disabled \
 && ninja -C build sway/sway

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
# Keep the distro's runtime dependencies and files, replacing only the
# compositor with the same release plus the layer-shell lifecycle fix.
COPY --from=sway-build /build/sway-1.10.1/build/sway/sway /usr/bin/sway
