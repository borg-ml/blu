set shell := ["bash", "-euo", "pipefail", "-c"]

upstream_revision := "f8ca77acdcb50241e3da21af663f8ef97b4b5ce4"
upstream_dir := ".upstream/luau"
upstream_build := ".upstream/build"

default:
    @just --list

upstream:
    @mkdir -p .upstream
    @if [ ! -d "{{upstream_dir}}/.git" ]; then git clone https://github.com/luau-lang/luau.git "{{upstream_dir}}"; fi
    @git -C "{{upstream_dir}}" fetch --depth 1 origin "{{upstream_revision}}"
    @git -C "{{upstream_dir}}" checkout --detach "{{upstream_revision}}"
    @cmake -S "{{upstream_dir}}" -B "{{upstream_build}}" -DCMAKE_BUILD_TYPE=Release -DLUAU_BUILD_TESTS=OFF
    @cmake --build "{{upstream_build}}" --target Luau.Repl.CLI Luau.Compile.CLI --parallel

test:
    cargo test --workspace

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

conformance: upstream
    cargo run -p blu-conformance -- --upstream "{{upstream_build}}/luau" --source "{{upstream_dir}}"
