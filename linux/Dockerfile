# Builds the Linux (Ubuntu, amd64) bundles: .deb and .AppImage
FROM --platform=linux/amd64 ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential curl wget file ca-certificates pkg-config xz-utils \
      libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev \
      libssl-dev libudev-dev libusb-1.0-0-dev patchelf squashfs-tools \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://nodejs.org/dist/v20.18.1/node-v20.18.1-linux-x64.tar.xz | tar -xJ -C /usr/local --strip-components=1
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
ENV PATH=/root/.cargo/bin:$PATH \
    APPIMAGE_EXTRACT_AND_RUN=1 \
    NO_STRIP=true
WORKDIR /src
COPY package.json package-lock.json ./
RUN npm ci
COPY . .
RUN npm run tauri build -- --bundles deb,appimage
