# Everything runs in Docker; the host needs only docker, make and git.
#   make                 the loader and the test kernel for ARCH (x86_64 or aarch64)
#   make check           host tests of the core
#   make test            QEMU cases for ARCH (CASES=... to pick)
#   make notices         out/release/NOTICES-ARCH.txt: what the loader has compiled in
IMG      := glassine-loader:build
KIMG     := glassine-loader:kernel
ARCH     ?= x86_64
TARGET   := $(ARCH)-unknown-uefi
KVM      := $(shell [ -w /dev/kvm ] && echo --device /dev/kvm --group-add $$(stat -c %g /dev/kvm))
RUN      := docker run --rm $(KVM) --user $(shell id -u):$(shell id -g) -v $(CURDIR):/work -w /work $(IMG)
EFI      := target/$(TARGET)/release/glassine-loader.efi

.PHONY: all image loader check mkboot payload test notices clean distclean
all: loader

image: .tmp/image.stamp
.tmp/image.stamp: docker/Dockerfile docker/Dockerfile.kernel
	mkdir -p .tmp
	docker build -q -t $(IMG) -f docker/Dockerfile docker
	docker build -q -t $(KIMG) -f docker/Dockerfile.kernel docker
	touch $@

third_party/lvgl-9.6.0/.ok: scripts/fetch-lvgl.sh | image
	$(RUN) scripts/fetch-lvgl.sh

loader: image third_party/lvgl-9.6.0/.ok
	$(RUN) cargo build --release --locked --target $(TARGET) -p glassine-loader -p glassine-test-kernel
	@ls -l $(EFI) | awk '{print "loader ($(ARCH)):", $$5, "bytes"}'
	@sha256sum $(EFI)

check: image
	$(RUN) cargo test --locked -p glassine-loader-core

mkboot: image
	$(RUN) sh -c 'cd tests/mkboot && cargo build --release --locked'

payload: out/payload/vmlinuz
out/payload/vmlinuz: image scripts/kernel-payload.sh initramfs/init
	mkdir -p out/payload
	docker run --rm -v $(CURDIR):/src:ro -v $(CURDIR)/out/payload:/o $(KIMG) \
	    sh -c '/src/scripts/kernel-payload.sh /o && chown -R $(shell id -u):$(shell id -g) /o'

test: loader mkboot $(if $(filter x86_64,$(ARCH)),payload)
	$(RUN) python3 -I tests/qemu/run.py --arch $(ARCH) $(CASES)

notices: loader
	$(RUN) scripts/notices.sh $(ARCH)

clean:
	rm -rf target tests/mkboot/target out .tmp

distclean: clean
	rm -rf .cache third_party/lvgl-9.6.0 third_party/*.tar.gz
	-docker rmi $(IMG) $(KIMG)
