# ---------------------------
# Stage 1: Build the Rust app
# ---------------------------
FROM rust:1.90-bookworm AS builder

# Install build dependencies (adjust as needed for your project)
# - pkg-config/libssl-dev are common for crates using OpenSSL
# - git is required to clone the repository
RUN set -eux; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      git pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Build arguments to parameterize the build
# REPO: Git URL (e.g. https://github.com/owner/repo.git)
# REF:  Branch / tag / commit to checkout (default: main)
# BIN_NAME: the final binary name produced by cargo
# FEATURES: optional cargo features (space/comma separated)
# PROFILE: release or debug (default: release)
ARG REPO=https://github.com/JoverZhang/tg-watchbot.git
ARG REF=master
ARG BIN_NAME=tg-watchbot
ARG FEATURES=""
ARG PROFILE=release

# Use a dedicated workdir
WORKDIR /src

# Clone only the specified ref (shallow clone for speed)
# Note: if REF is a commit SHA not on HEAD, remove --branch and set --depth appropriately.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    git clone --depth 1 --branch "${REF}" "${REPO}" app

WORKDIR /src/app

# Build the project
# --locked enforces Cargo.lock (recommended for reproducible builds)
# PROFILE flag mapping: cargo uses --release, while debug is default
# We map ${PROFILE} to cargo flags accordingly.
# If you use workspaces with multiple bins, ensure BIN_NAME matches Cargo.toml
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    bash -lc '\
      set -eux; \
      if [ "${PROFILE}" = "release" ]; then \
        cargo build --locked --release ${FEATURES:+--features ${FEATURES}}; \
        echo "target path: target/release/${BIN_NAME}"; \
        test -x "target/release/${BIN_NAME}"; \
      else \
        cargo build --locked ${FEATURES:+--features ${FEATURES}}; \
        echo "target path: target/debug/${BIN_NAME}"; \
        test -x "target/debug/${BIN_NAME}"; \
      fi \
    '

# ---------------------------
# Stage 2: Runtime with FFmpeg
# ---------------------------
FROM debian:bookworm-slim AS runtime

# Install ffmpeg and minimal runtime libraries
# - libssl3 is needed if your app is dynamically linked against OpenSSL 3
# - tzdata for correct time handling (optional)
RUN set -eux; \
    apt-get update; \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      ffmpeg libssl3 ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user
RUN useradd -m -u 1000 appuser

WORKDIR /app

# Build args must be re-declared to use them again in this stage (for copy path)
ARG PROFILE=release
ARG BIN_NAME

# Copy the compiled binary from the builder stage
# The path depends on the selected profile
COPY --from=builder /src/app/target/${PROFILE}/${BIN_NAME} /usr/local/bin/app

# Ensure it’s executable
RUN chmod +x /usr/local/bin/app

# Healthcheck to verify both ffmpeg and the app are available
HEALTHCHECK --interval=30s --timeout=5s --retries=3 CMD bash -lc 'ffmpeg -version >/dev/null 2>&1 && /usr/local/bin/app --help >/dev/null 2>&1 || exit 1'

# Drop privileges
USER appuser

# Environment (adjust for your app)
ENV RUST_LOG=info

# Default entrypoint launches your app; ffmpeg is available in PATH
ENTRYPOINT ["/usr/local/bin/app"]
# CMD ["--help"]  # optionally provide default args
