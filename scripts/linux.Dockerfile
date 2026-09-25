# The Linux environment scripts/linux-cargo runs in.
FROM rust:1.96-bookworm
RUN rustup component add clippy rustfmt
