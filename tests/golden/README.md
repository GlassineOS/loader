# Golden files

`pc-boot/` is copied unchanged from GlassineOS/installer at commit
`e44e0e2bcf73c922686aece45d92e636d0801e52` (`core/tests/golden/pc-boot/`): the loader
record and the set records the installer writes. The loader's host tests read them, so
both programs agree on the bytes.
