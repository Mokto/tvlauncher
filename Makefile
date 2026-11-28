WEBOS_SDK ?= $(HOME)/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot
TV        ?= lg-c1
DEST      ?= /media/developer/tvlauncher

NIGHTLY ?= nightly
CARGO   = PATH="$(HOME)/.cargo/bin:$$PATH" cargo
ARM_BIN = target/arm-unknown-linux-gnueabi/release/tvlauncher

# The TV build needs nightly: -Zbuild-std rebuilds std with the codegen flags from .cargo/config.toml.
# Do not set RUSTFLAGS here, it would replace them.
arm:
	WEBOS_SDK=$(WEBOS_SDK) CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABI_LINKER=$(CURDIR)/tools/arm-gcc \
	  $(CARGO) +$(NIGHTLY) build --release --target arm-unknown-linux-gnueabi -Zbuild-std=std,panic_abort

sim: fixtures/luna/listLaunchPoints.json
	LAUNCHER_ROOT=$(CURDIR)/fixtures $(CARGO) run --release

# KEYS: R/L/U/D presses before the capture, e.g. `make shot KEYS=RRD`.
shot: fixtures/luna/listLaunchPoints.json
	@mkdir -p build
	LAUNCHER_ROOT=$(CURDIR)/fixtures $(CARGO) run --release -- --shot build/shot.bmp $(KEYS)
	sips -s format png build/shot.bmp --out build/shot.png >/dev/null && echo build/shot.png

fixtures/luna/listLaunchPoints.json:
	sh tools/pull-fixtures.sh $(TV)

# The FFmpeg libraries of the film player (LGPL, built from FFmpeg's own source; see tools/build-ffmpeg.sh).
FFMPEG_ARM = build/ffmpeg-arm/libavformat-plx.so.63
$(FFMPEG_ARM):
	sh tools/build-ffmpeg.sh arm

ffmpeg-host:
	sh tools/build-ffmpeg.sh host

# libass for styled ASS subtitles (tools/build-libass.sh): ISC, with FreeType, HarfBuzz and FriBidi inside.
LIBASS_ARM = build/libass-arm/libass-plx.so.0
$(LIBASS_ARM):
	sh tools/build-libass.sh arm

libass-host:
	sh tools/build-libass.sh host

# The kernel module that lets the film player send TrueHD and full-rate DTS-HD to a receiver: a release of
# https://github.com/Mokto/earc_route (GPL-2.0-only), pinned by SHA-256, with its source for the package.
EARC_VER     = 0.1.0
EARC_TGZ_SHA = 1c32b01fb30a9d365b51ed0b540c83a6dae045ffae49642176391d9bd03559f5
EARC_SRC_SHA = 39de8a35e416cb7cc663b6cbf97370743540d6cf1f6f8936752fb2e01c614eb6
EARC_URL     = https://github.com/Mokto/earc_route/releases/download/v$(EARC_VER)
EARC_DIR     = build/earc-route-hbr-$(EARC_VER)
EARC_SRC     = build/earc_route-$(EARC_VER)-source.tar.gz
$(EARC_DIR)/earc_route-4.4.ko:
	mkdir -p build
	curl -fL --retry 3 --retry-all-errors -o build/earc-route-hbr.tar.gz $(EARC_URL)/earc-route-hbr-$(EARC_VER).tar.gz
	echo "$(EARC_TGZ_SHA)  build/earc-route-hbr.tar.gz" | shasum -a 256 -c -
	tar -C build -xzf build/earc-route-hbr.tar.gz
$(EARC_SRC):
	mkdir -p build
	curl -fL --retry 3 --retry-all-errors -o $@ $(EARC_URL)/earc_route-$(EARC_VER)-source.tar.gz
	echo "$(EARC_SRC_SHA)  $@" | shasum -a 256 -c -
HBR_FILES = $(EARC_DIR)/* hbr/hbr.sh

deploy: arm $(FFMPEG_ARM) $(LIBASS_ARM) $(EARC_DIR)/earc_route-4.4.ko
	ssh $(TV) 'mkdir -p $(DEST)/lib $(DEST)/hbr && rm -f $(DEST)/tvlauncher.new'
	scp $(ARM_BIN) $(TV):$(DEST)/tvlauncher.new
	scp takeover.sh debloat.sh boot.sh $(TV):$(DEST)/
	scp build/ffmpeg-arm/*.so.* $(LIBASS_ARM) $(TV):$(DEST)/lib/
	scp $(HBR_FILES) $(TV):$(DEST)/hbr/
	ssh $(TV) 'chmod 755 $(DEST)/tvlauncher.new && mv $(DEST)/tvlauncher.new $(DEST)/tvlauncher'

# The ARM build under QEMU, without a TV (needs Docker).
qemu: arm
	sh tools/qemu-smoke.sh

on:  ; ssh $(TV) sh $(DEST)/takeover.sh on

# Start at every boot, instead of CleanYourTV's Home: CleanYourTV's own DISABLE switch (its service keeps running)
# plus a hook in webosbrew's init.d. `make unboot` puts CleanYourTV's Home back from the next boot.
boot: ; ssh $(TV) sh $(DEST)/takeover.sh boot on
unboot: ; ssh $(TV) sh $(DEST)/takeover.sh boot off
off: ; ssh $(TV) sh $(DEST)/takeover.sh off

# A package for people without the toolchain: the prebuilt binary, the scripts and install.sh.
VERSION = $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
PKG     = tvlauncher-$(VERSION)-webos-arm
release: arm $(FFMPEG_ARM) $(LIBASS_ARM) $(EARC_DIR)/earc_route-4.4.ko $(EARC_SRC)
	rm -rf dist/$(PKG) && mkdir -p dist/$(PKG)/lib dist/$(PKG)/hbr
	cp $(ARM_BIN) takeover.sh debloat.sh boot.sh install.sh LICENSE README.md THIRD-PARTY-NOTICES.md dist/$(PKG)/
	cp build/ffmpeg-arm/*.so.* $(LIBASS_ARM) dist/$(PKG)/lib/
	tar -C build/libass-src -cJf dist/libass-sources.tar.xz .
	cp $(HBR_FILES) dist/$(PKG)/hbr/
	cp $(EARC_SRC) dist/$(PKG)/
	cp build/ffmpeg-*.tar.xz dist/ 2>/dev/null || true
	tar -C dist -czf dist/$(PKG).tar.gz $(PKG)
	cd dist && shasum -a 256 $(PKG).tar.gz > $(PKG).tar.gz.sha256 && cat $(PKG).tar.gz.sha256

.PHONY: arm sim shot deploy release qemu on off boot unboot ffmpeg-host libass-host
