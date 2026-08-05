#!/bin/bash
# Build one A/B bench witness image.
#
#   build_witness.sh <link-base> <witness-addr> <witness-value> \
#                    <other-witness-addr> <bootreq-addr> <out.bin>
#
# Intermediates go to a temp dir so running this never leaves anything in the
# source tree. MRS_TOOLCHAIN overrides the compiler location.
set -euo pipefail

if [ $# -ne 6 ]; then
    sed -n '2,8p' "$0" >&2
    exit 2
fi

TOOLCHAIN="${MRS_TOOLCHAIN:-$HOME/Development/Mounriver/Toolchain/RISC-V Embedded GCC12/bin}"
GCC="$TOOLCHAIN/riscv-wch-elf-gcc"
OBJCOPY="$TOOLCHAIN/riscv-wch-elf-objcopy"
[ -x "$GCC" ] || { echo "no riscv-wch-elf-gcc under $TOOLCHAIN; set MRS_TOOLCHAIN" >&2; exit 1; }

SRC="$(cd "$(dirname "$0")" && pwd)/witness.S"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "SECTIONS { . = $1; .text : { *(.text) } }" > "$TMP/link.ld"
"$GCC" -march=rv32imac_zicsr_zifencei -mabi=ilp32 -nostdlib \
    -DWITNESS_ADDR="$2" -DWITNESS_VALUE="$3" -DOTHER_ADDR="$4" -DBOOTREQ_ADDR="$5" \
    -Wl,-T,"$TMP/link.ld" -o "$TMP/witness.elf" "$SRC"
"$OBJCOPY" -O binary "$TMP/witness.elf" "$6"
printf "  %-14s base=%-9s %s <- %s\n" "$(basename "$6")" "$1" "$2" "$3"
