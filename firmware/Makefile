# OpenBoot build entry point: one Makefile, an eight-image matrix.
#
# OpenBoot is an 8 KiB IAP bootloader living in flash [0x0000, 0x2000) on WCH
# CH570/CH572/CH591/CH592 parts; the application starts at 0x2000. One image
# is built per CHIP x TRANSPORT combination:
#
#   CHIP      = ch570 | ch572 | ch591 | ch592
#   TRANSPORT = usb | uart        (exactly one transport is linked per image)
#
#   make                            image for the default CHIP/TRANSPORT
#   make CHIP=ch570 TRANSPORT=usb   one specific image
#   make ch570-usb                  the same image, as a convenience target
#   make all                        the full 8-image matrix
#   make matrix-report              the matrix plus a size table
#   make flash                      write the current image via minichlink (SWD)
#   make flash-isp                  write the current image via wchisp (ROM ISP)
#   make test                       host-native core tests, no cross toolchain
#   make check-deps                 verify pinned SDKs + toolchain
#   make print-image-path           absolute path of the current .bin, nothing else
#   make factory APP=app.bin        whole-chip image: bootloader + pad + app
#
# Every image must fit the 8 KiB bootloader region. That is enforced three
# ways: the linker script's FLASH MEMORY length, the post-objcopy byte gate in
# the .bin rule below, and the matrix-report table.

# Grouped targets and other modern behaviors need GNU Make 4.3. An older Make
# misparses the syntax and does something silently wrong instead of refusing,
# so refuse up front rather than produce artifacts nobody can explain.
ifneq ($(shell printf '4.3\n%s\n' '$(MAKE_VERSION)' | sort -V -C && echo ok),ok)
  $(error GNU Make 4.3 or newer is required; this is $(MAKE_VERSION))
endif

SHELL := /bin/sh
.SHELLFLAGS := -eu -c
# Delete a target whose recipe fails. Load-bearing for the size gate below: an
# oversize .bin must not survive on disk where a later `make flash` would
# happily program it over the bootloader region.
.DELETE_ON_ERROR:

# ---------------------------------------------------------------------------
# The build matrix.
#
# CHIP selects the flash geometry (app end address) and the HELLO family id.
# TRANSPORT selects the one transport .c linked into the image and the CPU
# clock: UART images run entirely at the 6.4 MHz boot clock (115200 baud fits
# with DL=7, -0.79% error, and libISP flash ops are proven to work there), so
# they need no clock init at all. USB images use the PLL: 100 MHz on ch57x
# and 60 MHz on ch59x.
#
# The default is CH592 UART, which leaves the debugger enabled.
# ---------------------------------------------------------------------------
CHIPS := ch570 ch572 ch591 ch592
TRANSPORTS := usb uart

CHIP ?= ch592
TRANSPORT ?= uart

ifeq ($(filter $(CHIP),$(CHIPS)),)
  $(error CHIP must be one of {$(CHIPS)}, got "$(CHIP)")
endif
ifeq ($(filter $(TRANSPORT),$(TRANSPORTS)),)
  $(error TRANSPORT must be one of {$(TRANSPORTS)}, got "$(TRANSPORT)")
endif

# Chip -> port family. CH570/CH572 share the ch57x port, CH591/CH592 the
# ch59x port; everything variant-specific below the family level is injected
# as a -D (see DEFINES) so the port sources stay variant-agnostic.
PORT_ch570 := ch57x
PORT_ch572 := ch57x
PORT_ch591 := ch59x
PORT_ch592 := ch59x
PORT := $(PORT_$(CHIP))

# Exclusive end of the application flash region, injected per VARIANT as
# -DOB_FLASH_APP_END (openboot_port.h #errors if it is missing):
#   ch570/ch572: 240 K part, app [0x2000, 0x3C000) -- all of CodeFlash, since
#                records now live inside their slots rather than in a
#                reserved block (see docs/AB-UPDATE.md)
#   ch591:       192 K part, app [0x2000, 0x30000)
#   ch592:       448 K part, app [0x2000, 0x70000)
APP_END_ch570 := 0x0003C000
APP_END_ch572 := 0x0003C000
APP_END_ch591 := 0x00030000
APP_END_ch592 := 0x00070000
# `override` on both assignments: a variable set on the make COMMAND LINE
# normally beats every assignment in the makefile, so without it
#   make BOARD=opendongle-ch570 APP_END=0x0003B000
# would silently defeat the board's validated clamp and hand OBP the dongle's
# RF bond page. APP_END is Makefile-internal; OB_APP_END is the knob, and it
# is validated below.
override APP_END := $(APP_END_$(CHIP))
# The app base is fixed at OB_APP_BASE for every chip; used below to bound a
# board override. The C side is the safety net (openboot_port.h and the port
# headers #error on an empty or overlapping region) — this is only here to
# say so in Make's voice, before a confusing compile error.
APP_BASE_BYTES := 8192
# Flash erase granularity, and therefore the alignment every region boundary
# must respect. Mirrors OB_FLASH_ERASE_BLOCK in the port headers.
ERASE_BLOCK_BYTES := 4096

# HELLO chip-family id (names from protocol/openboot_protocol.h).
FAMILY_ch570 := OB_FAMILY_CH570
FAMILY_ch572 := OB_FAMILY_CH572
FAMILY_ch591 := OB_FAMILY_CH591
FAMILY_ch592 := OB_FAMILY_CH592
CHIP_FAMILY := $(FAMILY_$(CHIP))

# HELLO transport id and the CPU clock the image runs at (rationale above).
TRANSPORT_ID_usb := OB_TRANSPORT_ID_USB
TRANSPORT_ID_uart := OB_TRANSPORT_ID_UART
TRANSPORT_ID := $(TRANSPORT_ID_$(TRANSPORT))
# USB clock is per FAMILY, not shared: every WCH EVT USB example for ch57x
# runs CLK_SOURCE_HSE_PLL_100MHz (0x46), and at 60 MHz the ch570 PHY
# enumerates but is electrically unstable (hub reports "disabled by hub
# (EMI?)" and redetects it as low-speed). ch59x USB remains at 60 MHz.
CPU_HZ_usb_ch57x := 100000000
CPU_HZ_usb_ch59x := 60000000
CPU_HZ_uart_ch57x := 6400000
CPU_HZ_uart_ch59x := 6400000
CPU_HZ := $(CPU_HZ_$(TRANSPORT)_$(PORT))

# RAM geometry for the linker. The Makefile owns the per-family RAM size
# (it already owns the per-chip flash geometry above); the linker derives
# _eusrstack (top of RAM minus the reserved 16 bytes) from it, and
# openboot.ld ASSERTs that equals the family's hand-written boot-request
# constant below — the one cross-check keeping RAM_SIZE and the protocol
# header from drifting apart while the app companion still uses the
# constant at runtime.
RAM_SIZE_ch57x := 12K
RAM_SIZE_ch59x := 26K
RAM_SIZE := $(RAM_SIZE_$(PORT))
BOOTREQ_NAME_ch57x := OB_BOOTREQ_ADDR_CH57X
BOOTREQ_NAME_ch59x := OB_BOOTREQ_ADDR_CH59X
BOOTREQ_NAME := $(BOOTREQ_NAME_$(PORT))
PROTOCOL_H := $(abspath ../protocol/openboot_protocol.h)
# Per-family microarchitecture CSRs injected into startup_ch5xx.S; the
# values are the WCH EVT startup files' (startup_CH572.S / startup_CH592.S):
# 0xbc0 corecfgr (pipeline/prefetch) is 0x25 on ch57x, 0x1f on ch59x, and
# only ch57x also writes 0xbc1=1. intsyscr (0x804) is 0x3 for both families
# and stays literal in startup_ch5xx.S.
STARTUP_CORECFGR_ch57x := 0x25
STARTUP_CORECFGR_ch59x := 0x1f
STARTUP_WRITE_BC1_ch57x := 1
STARTUP_WRITE_BC1_ch59x := 0
REPO_ROOT := $(abspath ..)

# Pinned openwch EVT trees (git submodules; check-deps verifies the pins).
# Only headers and libISP come from here -- no SDK driver .c is compiled.
SDK_ch57x := $(abspath ../third_party/openwch/ch570/EVT/EXAM)
SDK_ch59x := $(abspath ../third_party/openwch/ch592/EVT/EXAM)
SDK ?= $(SDK_$(PORT))

# The ROM flash API archive — the only binary linked from the SDK, and only
# when FLASH_DRIVER=isp selects it (below).
ISPLIB_ch57x := ISP572
ISPLIB_ch59x := ISP592
ISPLIB := $(ISPLIB_$(PORT))
ISPLIB_A := $(SDK)/SRC/StdPeriphDriver/lib$(ISPLIB).a

# ---------------------------------------------------------------------------
# Flash driver. `open` (default) is ports/flash_ch5xx.c, the open
# reimplementation; `isp` links the closed vendor archive instead. The knob
# exists only for side-by-side hardware validation of the open driver and is
# deleted — along with the whole isp path — once that evidence lands.
# ---------------------------------------------------------------------------
FLASH_DRIVER ?= open
# Exact single-word match: $(filter) with the user value as the pattern would
# accept 'open isp' (and then split the variant selection below).
ifneq ($(words $(FLASH_DRIVER)),1)
  $(error FLASH_DRIVER must be 'open' or 'isp' (got '$(FLASH_DRIVER)'))
endif
ifeq ($(filter open isp,$(FLASH_DRIVER)),)
  $(error FLASH_DRIVER must be 'open' or 'isp' (got '$(FLASH_DRIVER)'))
endif
ifeq ($(FLASH_DRIVER),isp)
  OB_FLASH_DRIVER_ISP := 1
else
  OB_FLASH_DRIVER_ISP := 0
endif

# ---------------------------------------------------------------------------
# Board configuration. A board .mk provides the boot-strap pin and the idle
# auto-boot timeout; BOARD is overridable so real hardware can ship its own
# file without touching this Makefile:  make BOARD=myboard CHIP=ch592 ...
# ---------------------------------------------------------------------------
BOARD ?= generic-$(PORT)
include boards/$(BOARD).mk
# Fallback if a board file omits the timeout; 0 disables idle auto-boot.
OB_IDLE_TIMEOUT_MS ?= 10000
# A board that defines only a strap mask selects port A by default.
OB_BOOT_PIN_PORT_B ?= 0
OB_UART1_REMAP ?= 0
OB_BOOT_IMAGE_CRC ?= 0

# Boolean knobs must be exactly the single word 0 or 1. A bare membership
# $(filter) is not enough: its pattern list is word-split, so a mangled
# value like '0 1' would pass — the $(words) term closes that hole.
$(foreach knob,OB_BOOT_PIN_PORT_B OB_UART1_REMAP OB_BOOT_IMAGE_CRC,\
  $(if $(filter-out 0 1,$($(knob)))$(filter-out 1,$(words $($(knob)))),\
    $(error $(knob) must be 0 or 1 (got '$($(knob))'))))

# A board may pin the clock the bootloader runs at — and therefore hands to
# the application (see ob_jump_app's CONTRACT in core/openboot_port.h).
# UART builds only: USB needs its family PLL value to function. The port's
# clock init #errors on any value it has no bring-up sequence for, so a
# wrong frequency fails the compile, not the bench.
ifneq ($(strip $(OB_CPU_HZ)),)
ifneq ($(TRANSPORT),uart)
$(error OB_CPU_HZ is only settable on UART builds; USB requires the family PLL clock)
endif
ifneq ($(words $(OB_CPU_HZ)),1)
$(error OB_CPU_HZ must be a single frequency in Hz (got '$(OB_CPU_HZ)'))
endif
CPU_HZ := $(strip $(OB_CPU_HZ))
endif

# A board may shrink the app region below what the silicon offers, to fence
# OBP off from flash it does not own. OpenDongle's CH570 keeps its RF bond at
# 0x3A000, directly below the boot-record page, so no ERASE/WRITE/COMMIT may
# reach either. The port's ob_app_end() takes min(silicon, build), so a bound
# set here also holds on larger silicon.
#
# Bounded rather than trusted: must be 4096-aligned (the erase granularity)
# and inside (app base, silicon end]. Board files are included above, after
# the per-chip default is assigned, which is what makes the override land.
ifneq ($(strip $(OB_APP_END)),)
  # Arithmetic expansion rather than printf '%d': POSIX requires $(( )) to
  # accept C integer constants including 0x hex, but does NOT require printf
  # to. A malformed value makes the expansion fail, the group produces no
  # output, and the $(error) below fires - the same outcome as before.
  APP_END_OK := $(shell \
    { want=$$(( $(strip $(OB_APP_END)) )) && \
      max=$$(( $(APP_END_$(CHIP)) )) && \
      [ "$$want" -gt $(APP_BASE_BYTES) ] && [ "$$want" -le "$$max" ] && \
      [ $$(( want % $(ERASE_BLOCK_BYTES) )) -eq 0 ] && echo ok; } 2>/dev/null)
  ifneq ($(strip $(APP_END_OK)),ok)
    $(error OB_APP_END=$(OB_APP_END) is not usable on $(CHIP): expected a \
            4096-aligned address above $(APP_BASE_BYTES) and no higher than \
            $(APP_END_$(CHIP)))
  endif
  override APP_END := $(OB_APP_END)
endif

# ---------------------------------------------------------------------------
# A/B slot geometry.
#
# The app region is split in two so a failed update leaves the previous image
# intact and bootable. Both slots are the same size because the application is
# LINKED per slot — these parts have no flash remap (the only address-related
# bit, RB_ROM_CODE_OFS, is a fixed boot-entry selector, not a translation), and
# the vendor BLE/RF libraries are -mcmodel=medany blobs whose PC-relative
# references to fixed RAM break under any base shift. So slot B holds a second
# build of the same application, not a copy of slot A's bytes.
#
# Size is half the region rounded DOWN to the erase granularity; any remainder
# is left unused at the top rather than making the slots unequal, which the
# per-slot link addresses could not express anyway.
#
#   ch570/ch572 (0x3C000): A [0x02000,0x1F000)  B [0x1F000,0x3C000)  116 KiB
#   ch570 clamped 0x3A000: A [0x02000,0x1E000)  B [0x1E000,0x3A000)  112 KiB
#   ch591       (0x30000): A [0x02000,0x19000)  B [0x19000,0x30000)   92 KiB
#   ch592       (0x70000): A [0x02000,0x39000)  B [0x39000,0x70000)  220 KiB
# ---------------------------------------------------------------------------
SLOT_GEOM := $(shell \
  base=$(APP_BASE_BYTES); \
  end=$$(printf '%d' '$(APP_END)'); \
  size=$$(( ((end - base) / 2 / $(ERASE_BLOCK_BYTES)) * $(ERASE_BLOCK_BYTES) )); \
  printf '0x%08X 0x%08X 0x%08X' "$$size" "$$base" "$$((base + size))")
OB_SLOT_SIZE   := $(word 1,$(SLOT_GEOM))
OB_SLOT_A_BASE := $(word 2,$(SLOT_GEOM))
OB_SLOT_B_BASE := $(word 3,$(SLOT_GEOM))

ifeq ($(strip $(OB_SLOT_SIZE)),0x00000000)
  $(error app region $(APP_BASE_BYTES)..$(APP_END) on $(CHIP) is too small to \
          split into two $(ERASE_BLOCK_BYTES)-byte-aligned slots)
endif

# ---------------------------------------------------------------------------
# Toolchain: a MounRiver "RISC-V Embedded GCC" install. The caller must point
# MRS_TOOLCHAIN at its bin directory. Every recipe use is quoted so paths may
# contain spaces (the stock install directories do):
#   make MRS_TOOLCHAIN=/path/to/bin ...
#
# GCC12 and GCC15 both build the matrix; check-deps warns rather than refuses
# on anything else. Note GCC15 renamed the tool prefix and emits slightly
# different code — see the TOOL_PREFIXES probe below and firmware/README.md.
# ---------------------------------------------------------------------------
MRS_TOOLCHAIN ?=

# Host-native tests and clean do not need the cross-toolchain. All public
# firmware targets do, including the default goal and matrix convenience
# targets, so fail at parse time with one actionable message.
TOOLCHAIN_GOALS := image all matrix-report board-policy check-deps flash flash-isp \
                   factory \
                   $(foreach c,$(CHIPS),$(foreach t,$(TRANSPORTS),$(c)-$(t)))
ifeq ($(strip $(MAKECMDGOALS)),)
  NEEDS_TOOLCHAIN := 1
else ifneq ($(strip $(filter $(TOOLCHAIN_GOALS),$(MAKECMDGOALS))),)
  NEEDS_TOOLCHAIN := 1
endif
ifeq ($(NEEDS_TOOLCHAIN),1)
  ifeq ($(strip $(MRS_TOOLCHAIN)),)
    $(error MRS_TOOLCHAIN is required; set it to a MounRiver RISC-V Embedded GCC bin directory)
  endif
endif

# Preserve the caller's host-compiler choice before CC becomes the
# cross-compiler. HOST_CC wins; an incoming CC is the compatibility fallback.
ifeq ($(strip $(HOST_CC)),)
  ifneq ($(filter default undefined,$(origin CC)),)
    HOST_CC := cc
  else
    HOST_CC := $(CC)
  endif
endif
export HOST_CC

# MounRiver renamed the tool prefix at GCC15: riscv-wch-elf-* became
# riscv32-wch-elf-*. Detect it instead of making every contributor bridge the
# rename with a symlink farm. Probed with $(shell) rather than $(wildcard)
# because MRS_TOOLCHAIN routinely contains a space ("RISC-V Embedded GCC12")
# and $(wildcard) splits on whitespace. Kept in sync with detect_tool_prefix()
# in check_dependencies.py, which enforces the same list.
TOOL_PREFIXES := riscv-wch-elf- riscv32-wch-elf-
ifneq ($(strip $(MRS_TOOLCHAIN)),)
  TOOL_PREFIX := $(shell for p in $(TOOL_PREFIXES); do \
                     if [ -x "$(MRS_TOOLCHAIN)/$${p}gcc" ]; then \
                       printf '%s' "$$p"; break; \
                     fi; \
                   done)
  ifeq ($(NEEDS_TOOLCHAIN)$(strip $(TOOL_PREFIX)),1)
    $(error no MounRiver compiler in "$(MRS_TOOLCHAIN)" (expected one of \
            $(addsuffix gcc,$(TOOL_PREFIXES))))
  endif
endif

CC := $(MRS_TOOLCHAIN)/$(TOOL_PREFIX)gcc
AS := $(CC)
LD := $(CC)
OBJCOPY := $(MRS_TOOLCHAIN)/$(TOOL_PREFIX)objcopy
SIZE := $(MRS_TOOLCHAIN)/$(TOOL_PREFIX)size
NM := $(MRS_TOOLCHAIN)/$(TOOL_PREFIX)nm
OBJDUMP := $(MRS_TOOLCHAIN)/$(TOOL_PREFIX)objdump

MINICHLINK ?= minichlink
WCHLINK_SERIAL ?=
WCHISP ?= wchisp
# Hermetic interpreter for the dependency gate; plain python3 for the host
# test harness, which needs the user's pytest.
PYTHON ?= /usr/bin/python3 -I
HOST_PYTHON ?= python3

# ---------------------------------------------------------------------------
# Artifact names: build/<chip>-<transport>/openboot-<chip>-<transport>.{elf,
# bin,hex,map}. Each matrix cell gets its own directory, so the whole matrix
# can coexist (and `make -j all` can build it in parallel).
# ---------------------------------------------------------------------------
# $(call build_dir,<chip>,<transport>,<board>) — the ONE place that knows the
# artifact layout. A non-default board gets its own directory so it can never
# reuse the default board's objects; the default keeps the documented
# build/<chip>-<transport> path unchanged.
build_dir = build/$(1)-$(2)$(if $(filter-out generic-$(PORT_$(1)),$(3)),+$(3))

TARGET := openboot-$(CHIP)-$(TRANSPORT)
# Only the non-default isp variant gets a keyed directory: the canonical
# paths stay stable for their consumers (tests/test_board_config.py,
# check_board_policy.py) while both variants can coexist for A/B validation.
BUILD := $(call build_dir,$(CHIP),$(TRANSPORT),$(BOARD))$(if $(filter isp,$(FLASH_DRIVER)),@isp)
MAX_IMAGE_BYTES := 8192

# ---------------------------------------------------------------------------
# Sources. The core is chip-agnostic (no SDK headers); one port and exactly
# one transport are compiled per image.
# ---------------------------------------------------------------------------
CORE_SRC := core/main.c core/crc32.c core/boot_core.c core/boot_decision.c
PORT_SRC := ports/port_ch5xx.c ports/flash_ch5xx.c ports/$(PORT)/port_$(PORT).c
TRANSPORT_SRC := transports/$(TRANSPORT)_transport.c
C_SRCS := $(CORE_SRC) $(PORT_SRC) $(TRANSPORT_SRC)
S_SRCS := ports/startup_ch5xx.S

# Objects are flattened into $(BUILD) by basename and found via VPATH; the
# guard keeps a future duplicate basename from silently colliding there.
C_OBJS := $(patsubst %.c,$(BUILD)/%.o,$(notdir $(C_SRCS)))
S_OBJS := $(patsubst %.S,$(BUILD)/%.o,$(notdir $(S_SRCS)))
OBJS := $(C_OBJS) $(S_OBJS)
DEPS := $(OBJS:.o=.d)

SOURCE_BASENAMES := $(notdir $(C_SRCS) $(S_SRCS))
ifneq ($(words $(SOURCE_BASENAMES)),$(words $(sort $(SOURCE_BASENAMES))))
  $(error duplicate source basename in the flattened object directory)
endif

VPATH := core:ports:ports/$(PORT):transports

# ---------------------------------------------------------------------------
# Flags.
# ---------------------------------------------------------------------------
# ch57x is a QingKe V3C-class core with the WCH extension set; ch59x is a
# plain RV32IMAC QingKe V4C. -msave-restore shares prologue/epilogue
# millicode from libgcc, reducing the image by more than 100 bytes.
ARCH_ch57x := -march=rv32imc_zba_zbb_zbc_zbs_xw -mabi=ilp32 -mcmodel=medany \
              -msmall-data-limit=8 -msave-restore
ARCH_ch59x := -march=rv32imac_zicsr_zifencei -mabi=ilp32 \
              -msmall-data-limit=8 -msave-restore
ARCH := $(ARCH_$(PORT))

# NO -flto, deliberately: the image links against the binary-only libISP
# archives from the EVT tree, which were not built for LTO. Mixing LTO
# objects with opaque archives risks symbol and section surprises in the
# .highcode flash primitives that must run from RAM.
CFLAGS := $(ARCH) -Os -g -std=gnu99
CFLAGS += -Wall -Wextra -Werror
CFLAGS += -ffunction-sections -fdata-sections -fno-common

INCLUDES := -I core -I ports/$(PORT)
# SFR + ISP headers only; no SDK driver sources are ever compiled.
INCLUDES += -I $(SDK)/SRC/StdPeriphDriver/inc

# The configuration contract (see core/openboot_port.h, which #errors when
# any of these is missing). Every value is emitted into a generated header
# that is force-included ahead of every translation unit, so no source file
# needs to include it and the host test harness — which supplies its own
# flags — is unaffected:
#   OB_PORT_HEADER   which port header the core includes.
#   OB_FLASH_APP_END per chip VARIANT (the port covers a family).
#   OB_CPU_HZ        per transport (boot clock vs PLL).
#   OB_CHIP_FAMILY   HELLO family id for this variant.
#   OB_TRANSPORT_ID  HELLO transport id for this image.
# The Boolean board knobs (OB_BOOT_PIN_PORT_B, OB_UART1_REMAP,
# OB_BOOT_IMAGE_CRC) are normalized to a literal 0 or 1 above, always
# emitted, and consumed with #if — never #ifdef, where defined-but-zero
# would read as enabled. Knobs that may be legitimately absent
# (OB_HSE_CAP_LOAD, OB_BOOT_PIN_MASK, the USB identity) still emit no
# line when unset, and their consumers #ifdef-test them.
CONFIG_H := $(BUILD)/openboot_config.h

CONFIG_LINES := \
  '/* Generated by firmware/Makefile for $(CHIP)-$(TRANSPORT), board $(BOARD). DO NOT EDIT. */' \
  '\#define OB_PORT_HEADER "port_$(PORT).h"' \
  '\#define OB_FLASH_APP_END $(APP_END)' \
  '\#define OB_CPU_HZ $(CPU_HZ)' \
  '\#define OB_CHIP_FAMILY $(CHIP_FAMILY)' \
  '\#define OB_TRANSPORT_ID $(TRANSPORT_ID)' \
  '\#define OB_FLASH_DRIVER_ISP $(OB_FLASH_DRIVER_ISP)' \
  '\#define OB_STARTUP_CORECFGR $(STARTUP_CORECFGR_$(PORT))' \
  '\#define OB_STARTUP_WRITE_BC1 $(STARTUP_WRITE_BC1_$(PORT))' \
  '\#define OB_IDLE_TIMEOUT_MS $(OB_IDLE_TIMEOUT_MS)' \
  '\#define OB_BOOT_PIN_PORT_B $(OB_BOOT_PIN_PORT_B)' \
  '\#define OB_UART1_REMAP $(OB_UART1_REMAP)' \
  '\#define OB_BOOT_IMAGE_CRC $(OB_BOOT_IMAGE_CRC)' \
  '\#define OB_SLOT_SIZE $(OB_SLOT_SIZE)' \
  '\#define OB_SLOT_A_BASE $(OB_SLOT_A_BASE)' \
  '\#define OB_SLOT_B_BASE $(OB_SLOT_B_BASE)'
# CH57x crystal load capacitance. Leave this absent to use the port's
# vendor-reference default; emit an explicit board override when supplied.
ifneq ($(strip $(OB_HSE_CAP_LOAD)),)
ifneq ($(PORT),ch57x)
$(error OB_HSE_CAP_LOAD is only valid for CH57x)
endif
ifneq ($(words $(strip $(OB_HSE_CAP_LOAD))),1)
$(error OB_HSE_CAP_LOAD must be one value from 0 to 7)
endif
ifeq ($(filter 0 1 2 3 4 5 6 7,$(strip $(OB_HSE_CAP_LOAD))),)
$(error OB_HSE_CAP_LOAD must be one value from 0 to 7)
endif
CONFIG_LINES += '\#define OB_HSE_CAP_LOAD $(OB_HSE_CAP_LOAD)'
endif
# A board that wants no strap leaves OB_BOOT_PIN_MASK unset and the port
# compiles ob_bootpin_asserted() to `return 0`.
ifneq ($(strip $(OB_BOOT_PIN_MASK)),)
CONFIG_LINES += '\#define OB_BOOT_PIN_MASK $(OB_BOOT_PIN_MASK)'
endif
# USB identity. Absent leaves the pid.codes test allocation compiled into
# usb_transport.c. A product sets both so its bootloader enumerates under the
# same identity as its application; the host then needs the HID usage page to
# tell the two apart (docs/PROTOCOL.md section 12).
ifneq ($(strip $(OB_USB_VID))$(strip $(OB_USB_PID)),)
ifeq ($(strip $(OB_USB_VID)),)
$(error set OB_USB_VID alongside OB_USB_PID, or neither)
endif
ifeq ($(strip $(OB_USB_PID)),)
$(error set OB_USB_PID alongside OB_USB_VID, or neither)
endif
CONFIG_LINES += '\#define OB_USB_VID $(OB_USB_VID)' \
                '\#define OB_USB_PID $(OB_USB_PID)'
endif

# Force-include the generated configuration ahead of every TU. Equivalent to
# the -D forest it replaces: both establish the macros before line 1 of the
# source, so the object code is unchanged (the byte-identity gate in the
# commit message proves it). CFLAGS is simply-expanded, so this must come
# after CONFIG_H is defined.
CFLAGS += -include $(CONFIG_H)

LDFLAGS := $(ARCH) -T ports/openboot.ld -nostartfiles
LDFLAGS += --specs=nano.specs --specs=nosys.specs
LDFLAGS += -Wl,--gc-sections -Wl,--print-memory-usage
LDFLAGS += -Wl,-Map=$(BUILD)/$(TARGET).map
LDFLAGS += -L $(BUILD)          # INCLUDE memory.ld
# Only isp builds may reference the vendor archive: the open link carrying no
# -L into the SDK and no -lISP is the proof it depends on nothing closed.
ifeq ($(FLASH_DRIVER),isp)
LDFLAGS += -L $(SDK)/SRC/StdPeriphDriver
LDLIBS := -l$(ISPLIB)
ISPLIB_PREREQ := $(ISPLIB_A)
else
LDLIBS :=
ISPLIB_PREREQ :=
endif

# ---------------------------------------------------------------------------
# Targets.
# ---------------------------------------------------------------------------
.DEFAULT_GOAL := image
MATRIX := $(foreach c,$(CHIPS),$(foreach t,$(TRANSPORTS),$(c)-$(t)))
.PHONY: board-policy image all matrix-report clean check-deps test flash flash-isp \
        print-image-path factory $(MATRIX)

image: check-deps $(BUILD)/$(TARGET).bin $(BUILD)/$(TARGET).hex \
       $(BUILD)/$(TARGET).manifest

# Where the current CHIP/TRANSPORT/BOARD image lands, for consumers that
# package the bootloader (a superproject composing a factory image, say).
# The build_dir template above is the only definition of that layout, and a
# consumer hardcoding it breaks silently the day the template changes.
#
# Contract: prints one absolute path and NOTHING else, builds nothing, and is
# deliberately absent from TOOLCHAIN_GOALS so it answers without
# MRS_TOOLCHAIN set. The .elf/.hex/.map sit beside it under the same stem.
# Callers must pass --no-print-directory, because make's own "Entering
# directory" lines go to stdout and would otherwise land in $(shell ...).
print-image-path:
	@echo "$(abspath $(BUILD)/$(TARGET).bin)"

# The full matrix, one recursive make per cell so each gets its own CHIP/
# TRANSPORT (and thus its own flag set and build directory). The `+` marks
# the recursion for the jobserver, so `make -j8 all` parallelizes across
# cells and `make -n all` dry-runs into them.
all: $(MATRIX)

$(MATRIX):
	+$(MAKE) --no-print-directory \
	    CHIP=$(firstword $(subst -, ,$@)) \
	    TRANSPORT=$(lastword $(subst -, ,$@)) \
	    MRS_TOOLCHAIN="$(MRS_TOOLCHAIN)" image

# Size table over the whole matrix -- the third leg of the size gate, and
# the number to watch while the USB transport grows. Paths come from the
# same build_dir template the per-cell builds use, so the layout is defined
# in exactly one place.
# A BOARD= given on the command line reaches the recursive per-cell makes
# through MAKEFLAGS, so those cells build into build/<chip>-<transport>+<board>.
# The report has to look where the build actually went, or it reads whatever
# stale default-board binaries happen to be lying around and reports sizes for
# images it did not just build.
matrix_board = $(if $(filter command line,$(origin BOARD)),$(BOARD),generic-$(PORT_$(1)))
MATRIX_BINS := $(foreach c,$(CHIPS),$(foreach t,$(TRANSPORTS),\
  $(call build_dir,$(c),$(t),$(call matrix_board,$(c)))$(if $(filter isp,$(FLASH_DRIVER)),@isp)/openboot-$(c)-$(t).bin))

# No family ships a stay-in-bootloader strap — the mask-ROM ISP entry pin is
# the recovery mechanism (PA1, also USB D+, on ch57x; PB22 on ch59x). Getting
# that wrong is silent on the bench: a strap on a pin the package does not
# bond just reads its own pull-up and never asserts. So it is checked in the
# built artifact rather than the board file, right where the whole matrix is
# already in hand.
board-policy: all
	@$(HOST_PYTHON) check_board_policy.py --nm "$(NM)" --build build --board "$(BOARD)"

matrix-report: all board-policy
	@echo; \
	 printf '%-22s %s\n' "image" "bytes / limit"; \
	 printf '%s\n' "-------------------------------------"; \
	 for b in $(MATRIX_BINS); do \
	   n="$$(basename "$$b" .bin)"; n="$${n#openboot-}"; \
	   if [ -f "$$b" ]; then \
	     printf '%-22s %5s / $(MAX_IMAGE_BYTES)\n' "$$n" "$$(wc -c < "$$b")"; \
	   else \
	     printf '%-22s %s\n' "$$n" "missing"; \
	   fi; \
	 done

check-deps:
	$(PYTHON) check_dependencies.py --toolchain "$(MRS_TOOLCHAIN)"

# Host-native tests: compile the real core/*.c against a simulated flash and
# drive it from pytest. Needs a host C compiler and pytest, but no cross
# toolchain and no hardware.
test:
	$(HOST_PYTHON) ../protocol/gen_protocol.py --check
	$(HOST_PYTHON) tests/core_host/build.py
	$(HOST_PYTHON) -m pytest tests -q

$(BUILD):
	mkdir -p "$@"

# The generated config header IS the configuration, so every object depends
# on it and any knob change rebuilds everything. The recipe runs on every
# invocation but rewrites the file only when its contents actually change,
# so the mtime — and therefore the rebuild — is content-driven rather than
# timestamp-driven. That replaces the old parse-time content stamp: no
# $(file)/$(shell) side effects during Makefile evaluation.
.PHONY: FORCE
FORCE:

$(CONFIG_H): FORCE | $(BUILD)
	@printf '%s\n' $(CONFIG_LINES) > "$@.tmp"
	@if cmp -s "$@.tmp" "$@"; then rm -f "$@.tmp"; else mv -f "$@.tmp" "$@"; fi

# The linker's half of the generated configuration: the MEMORY block plus
# OB_BOOTREQ_ADDR, extracted from the protocol header (anchored sed, hard
# failure if the define ever moves or is renamed). openboot.ld computes
# _eusrstack from the MEMORY block and ASSERTs it equals this extracted
# address — the one cross-check tying RAM_SIZE to the protocol constant the
# runtime and the app companion use. FORCE + cmp for the same reason as the
# config header: the content depends on make variables (RAM_SIZE,
# BOOTREQ_NAME) as well as on files, and only regenerating unconditionally
# then comparing catches a variable override.
MEMORY_LD := $(BUILD)/memory.ld

$(MEMORY_LD): FORCE | $(BUILD)
	@addrs=$$(sed -n 's/^#define $(BOOTREQ_NAME)[[:space:]]\{1,\}\(0x[0-9A-Fa-f]\{1,\}\)[uU]\{0,1\}[[:space:]]*\(\/\*.*\)\{0,1\}$$/\1/p' "$(PROTOCOL_H)"); \
	 n=$$(printf '%s\n' "$$addrs" | grep -c . || true); \
	 if [ "$$n" != 1 ]; then \
	   echo "expected exactly one '#define $(BOOTREQ_NAME) 0x...' in $(PROTOCOL_H), found $$n" >&2; \
	   echo "(a second match usually means an old definition was left behind, commented out or inside an #if — sed cannot tell those from the live one, so refuse rather than guess)" >&2; \
	   exit 1; \
	 fi; \
	 addr=$$addrs; \
	 { echo "/* Generated by firmware/Makefile for $(CHIP). DO NOT EDIT. */"; \
	   echo "MEMORY"; \
	   echo "{"; \
	   echo "    FLASH (rx)  : ORIGIN = 0x00000000, LENGTH = 8K"; \
	   echo "    RAM   (xrw) : ORIGIN = 0x20000000, LENGTH = $(RAM_SIZE)"; \
	   echo "}"; \
	   echo "OB_BOOTREQ_ADDR = $$addr;"; \
	 } > "$@.tmp"
	@if cmp -s "$@.tmp" "$@"; then rm -f "$@.tmp"; else mv -f "$@.tmp" "$@"; fi

$(OBJS): Makefile $(CONFIG_H)

# The open flash driver must be RAM-self-contained: -msave-restore would emit
# calls into libgcc millicode in flash .text and a switch could emit a
# .rodata jump table, either of which is dead XIP mid-operation.
# check_highcode.py asserts the result on the linked ELF.
$(BUILD)/flash_ch5xx.o: CFLAGS += -mno-save-restore -fno-jump-tables

$(BUILD)/%.o: %.c | $(BUILD)
	"$(CC)" $(CFLAGS) $(INCLUDES) -MMD -MP -MF "$(@:.o=.d)" -MT "$@" -c "$<" -o "$@"

$(BUILD)/%.o: %.S | $(BUILD)
	"$(AS)" $(CFLAGS) $(INCLUDES) -MMD -MP -MF "$(@:.o=.d)" -MT "$@" -c "$<" -o "$@"

$(BUILD)/$(TARGET).elf: $(OBJS) ports/openboot.ld $(MEMORY_LD) $(ISPLIB_PREREQ)
	"$(LD)" $(LDFLAGS) $(OBJS) $(LDLIBS) -o "$@"
	"$(SIZE)" "$@"
	$(HOST_PYTHON) check_highcode.py --objdump "$(OBJDUMP)" --nm "$(NM)" \
	    --object "$(BUILD)/flash_ch5xx.o" --elf "$@"

# objcopy, then the HARD size gate. The linker MEMORY region already bounds
# the image, but a linker-script regression must not be the only line of
# defense for the one region that can brick the part. .DELETE_ON_ERROR
# removes the oversize .bin so it cannot be flashed by a later target.
$(BUILD)/$(TARGET).bin: $(BUILD)/$(TARGET).elf
	"$(OBJCOPY)" -O binary "$<" "$@"
	@actual=$$(wc -c < "$@"); \
	 if [ "$$actual" -gt $(MAX_IMAGE_BYTES) ]; then \
	   echo "$(TARGET).bin is $$actual bytes; the bootloader region is $(MAX_IMAGE_BYTES) (app starts at 0x2000)" >&2; \
	   exit 1; \
	 fi; \
	 echo "$(TARGET).bin: $$actual / $(MAX_IMAGE_BYTES) bytes"

$(BUILD)/$(TARGET).hex: $(BUILD)/$(TARGET).elf
	"$(OBJCOPY)" -O ihex "$<" "$@"

# Provenance for whoever packages this image. A superproject that vendors
# OpenBoot (composing a factory image, say) otherwise has no way to record
# WHICH bootloader it shipped: the gitlink is not consulted by any build step
# and no revision reaches the artifact.
#
# The image sha256 is the whole statement about configuration — it already
# subsumes every knob, source file and compiler choice — so this deliberately
# does not restate the knobs. It records what produced the bytes and what they
# hash to; openboot_config.h sits beside it for the expanded form.
#
# FORCE + cmp because the contents depend on git state, which moves without
# any file in the build tree changing.
$(BUILD)/$(TARGET).manifest: $(BUILD)/$(TARGET).bin FORCE
	@rev=$$(git -C "$(REPO_ROOT)" rev-parse HEAD 2>/dev/null || echo unknown); \
	 if [ "$$rev" = unknown ]; then dirty=unknown; \
	 elif [ -n "$$(git -C "$(REPO_ROOT)" status --porcelain 2>/dev/null)" ]; then dirty=1; \
	 else dirty=0; fi; \
	 if command -v sha256sum >/dev/null 2>&1; then sum=$$(sha256sum "$<"); \
	 else sum=$$(shasum -a 256 "$<"); fi; \
	 { echo "openboot_revision=$$rev"; \
	   echo "openboot_dirty=$$dirty"; \
	   echo "chip=$(CHIP)"; \
	   echo "transport=$(TRANSPORT)"; \
	   echo "board=$(BOARD)"; \
	   echo "image=$(notdir $<)"; \
	   echo "image_bytes=$$(wc -c < "$<" | tr -d ' ')"; \
	   echo "image_sha256=$${sum%% *}"; \
	 } > "$@.tmp"
	@if cmp -s "$@.tmp" "$@"; then rm -f "$@.tmp"; else mv -f "$@.tmp" "$@"; fi

# Whole-chip factory image: this bootloader, 0x00 pad to the app base, the
# caller's application, and slot A's boot record. One file to write at flash 0
# on a blank part, which then boots the application on its own.
#
#   make factory APP=/path/to/app.bin CHIP=ch592 TRANSPORT=usb
#
# The application must be the SLOT A build: the record describes slot A, and
# an application only runs at the base it was linked for.
#
# The record is what makes the part bootable off the line. Without it a
# factory-programmed device comes up in the bootloader and needs a host to run
# `openboot bless` before it will run anything — set FACTORY_BLESS=0 for that,
# which is occasionally what a bring-up image wants and never what production
# does.
#
# Blessing costs size: the record sits at the top of slot A, so the image
# spans everything below it and is mostly zeros. That is the price of one
# contiguous blob any programmer can write at address 0.
#
# The pad must be 0x00 and not 0xFF — programming 0xFF programs nothing on
# CH5xx, so an 0xFF pad never lands in flash and a post-flash readback compare
# fails across it. compose_factory.py owns that rule and the tests pin it.
FACTORY_BIN := $(BUILD)/$(TARGET)-factory.bin
FACTORY_BLESS ?= 1
# Usable image bytes in a slot: the top erase block belongs to its record.
OB_SLOT_CAPACITY := $(shell printf '0x%08X' \
  $$(( $(shell printf '%d' '$(OB_SLOT_SIZE)') - $(ERASE_BLOCK_BYTES) )))
FACTORY_BLESS_ARG := $(if $(filter-out 0,$(FACTORY_BLESS)),--bless-capacity $(OB_SLOT_CAPACITY))

# check-deps, not just the .bin: a factory image is the artifact that ships,
# so it must not be composable from an SDK or toolchain state that `make image`
# would have refused.
factory: check-deps $(BUILD)/$(TARGET).bin $(BUILD)/$(TARGET).manifest \
         compose_factory.py
	@test -n "$(strip $(APP))" || { \
	  echo "set APP=<application .bin> — factory needs an application to package" >&2; \
	  exit 1; }
	$(HOST_PYTHON) compose_factory.py --openboot "$(BUILD)/$(TARGET).bin" \
	    --app "$(APP)" --app-end $(APP_END) $(FACTORY_BLESS_ARG) \
	    --output "$(FACTORY_BIN)"

# SWD flash via a WCH-LinkE. minichlink ignores the return values of its
# CH5xx erase/write calls and its byte-for-byte verify is disabled, so a zero
# exit status is NOT evidence the image landed -- read it back and compare
# before rebooting the target. WCHLINK_SERIAL selects a probe when several
# are attached.
flash: image
	@command -v "$(MINICHLINK)" >/dev/null 2>&1 || { \
	  echo "minichlink not found: $(MINICHLINK)" >&2; exit 1; }
	"$(MINICHLINK)" -C linke \
	    $(if $(strip $(WCHLINK_SERIAL)),-l "$(WCHLINK_SERIAL)") \
	    -w "$(abspath $(BUILD)/$(TARGET).bin)" 0x0
	"$(MINICHLINK)" -C linke \
	    $(if $(strip $(WCHLINK_SERIAL)),-l "$(WCHLINK_SERIAL)") \
	    -r "$(BUILD)/.flash-readback.bin" 0x0 $$(wc -c < "$(BUILD)/$(TARGET).bin")
	cmp "$(BUILD)/.flash-readback.bin" "$(BUILD)/$(TARGET).bin"
	"$(MINICHLINK)" -C linke \
	    $(if $(strip $(WCHLINK_SERIAL)),-l "$(WCHLINK_SERIAL)") -b
	@echo "flashed and read back OK: $(TARGET).bin"

# ROM ISP flash via wchisp (chip in BOOT mode: hold the BOOT strap through a
# power cycle). This is also the recovery path for CH57x-USB images, where
# the USB PHY takes over the SWD pins and kills the debugger.
flash-isp: image
	@command -v "$(WCHISP)" >/dev/null 2>&1 || { \
	  echo "wchisp not found: $(WCHISP)" >&2; exit 1; }
	"$(WCHISP)" flash "$(BUILD)/$(TARGET).bin"

clean:
	rm -rf build

-include $(DEPS)
