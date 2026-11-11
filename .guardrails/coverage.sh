#!/usr/bin/env bash
# Coverage wrapper: cargo-llvm-cov needs the rustup toolchain's llvm-cov/llvm-profdata,
# matching the compiler that instruments the build (this box's distro rustc ships none).
set -euo pipefail
TC="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/lib/rustlib/x86_64-unknown-linux-gnu/bin"
export PATH="$HOME/.cargo/bin:$TC:$PATH"
export LLVM_COV="$TC/llvm-cov"
export LLVM_PROFDATA="$TC/llvm-profdata"
mkdir -p coverage
exec cargo llvm-cov --lcov --output-path coverage/lcov.info
