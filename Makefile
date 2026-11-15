# Everything runs in Docker; the host needs only docker and make.
IMG      := glassine-loader-spike:build
KIMG     := glassine-loader-spike:kernel
RUN      := docker run --rm --user $(shell id -u):$(shell id -g) -v $(CURDIR):/work -w /work $(IMG)
EFI      := target/x86_64-unknown-uefi/release/glassine-loader.efi

.PHONY: all image loader payload test clean distclean
all: loader

image: .tmp/image.stamp
.tmp/image.stamp: docker/Dockerfile docker/Dockerfile.kernel
	mkdir -p .tmp
	docker build -q --label org.glassine.spike=loader -t $(IMG) -f docker/Dockerfile docker
	docker build -q --label org.glassine.spike=loader -t $(KIMG) -f docker/Dockerfile.kernel docker
	touch $@

third_party/lvgl-9.6.0/.ok: scripts/fetch-lvgl.sh | image
	$(RUN) scripts/fetch-lvgl.sh

loader: image third_party/lvgl-9.6.0/.ok
	$(RUN) cargo build --release --locked
	@ls -l $(EFI) | awk '{print "loader:", $$5, "bytes"}'
	@sha256sum $(EFI)

payload: out/payload/vmlinuz
out/payload/vmlinuz: image scripts/kernel-payload.sh initramfs/init
	mkdir -p out/payload
	docker run --rm -v $(CURDIR):/src:ro -v $(CURDIR)/out/payload:/o $(KIMG) \
	    sh -c '/src/scripts/kernel-payload.sh /o && chown -R $(shell id -u):$(shell id -g) /o'

test: loader payload
	$(RUN) python3 -I tests/run.py $(CASES)

clean:
	rm -rf target out .tmp

distclean: clean
	rm -rf .cache third_party/lvgl-9.6.0 third_party/*.tar.gz
	-docker rmi $(IMG) $(KIMG)
