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

deploy: arm
	ssh $(TV) 'mkdir -p $(DEST) && rm -f $(DEST)/tvlauncher.new'
	scp $(ARM_BIN) $(TV):$(DEST)/tvlauncher.new
	scp takeover.sh debloat.sh boot.sh $(TV):$(DEST)/
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
release: arm
	rm -rf dist/$(PKG) && mkdir -p dist/$(PKG)
	cp $(ARM_BIN) takeover.sh debloat.sh boot.sh install.sh LICENSE README.md dist/$(PKG)/
	tar -C dist -czf dist/$(PKG).tar.gz $(PKG)
	cd dist && shasum -a 256 $(PKG).tar.gz > $(PKG).tar.gz.sha256 && cat $(PKG).tar.gz.sha256

.PHONY: arm sim shot deploy release qemu on off boot unboot
