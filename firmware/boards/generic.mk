# The bare bring-up / escape-hatch board — the default BOARD.
#
# It pins nothing, so it is what `make` (bare), `make CHIP=<chip>
# TRANSPORT=<transport>` (the dev-only raw escape hatch), and the
# config-generation tests all resolve to. With no `override` here, the
# Makefile's `CHIP ?= ch592` / `TRANSPORT ?= uart` defaults apply unless the
# command line overrides them.
#
# No product knobs (the Makefile's OB_IDLE_TIMEOUT_MS ?= 10000 fallback covers
# the one it needs). For a named per-cell bring-up image use one of the
# generic-<chip>-<transport> boards; for recovery see firmware/README.md.
