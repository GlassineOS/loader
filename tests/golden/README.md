# Golden files

`pc-boot/` is copied unchanged from GlassineOS/installer at commit
`da9e9d6f3c34a60ab67c3719c05aeb12c2dd0312` (`core/tests/golden/pc-boot/`): the loader
record and the set records the installer writes. The loader's host tests read them, so
both programs agree on the bytes.
