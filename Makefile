# Everything runs in Docker; the host needs only docker, make and git.
#   make check           host tests of the core
IMG      := glassine-loader:build
RUN      := docker run --rm --user $(shell id -u):$(shell id -g) -v $(CURDIR):/work -w /work $(IMG)

.PHONY: all image check clean distclean
all: check

image: .tmp/image.stamp
.tmp/image.stamp: docker/Dockerfile
	mkdir -p .tmp
	docker build -q -t $(IMG) -f docker/Dockerfile docker
	touch $@

check: image
	$(RUN) cargo test --locked -p glassine-loader-core

clean:
	rm -rf target out .tmp

distclean: clean
	rm -rf .cache
	-docker rmi $(IMG)
