#!/usr/bin/env bash
set -euo pipefail

binary=${1:-target/release/refuge}
repo_count=${REFUGE_BENCH_REPOS:-5}
snapshot_count=${REFUGE_BENCH_SNAPSHOTS:-20}
lfs_bytes=${REFUGE_BENCH_LFS_BYTES:-1048576}
bench_root=$(mktemp -d)
trap 'rm -rf "$bench_root"' EXIT

binary=$(realpath "$binary")
export REFUGE_CONFIG="$bench_root/config.toml"
export HOME="$bench_root/home"
export XDG_CONFIG_HOME="$bench_root/xdg"
export GIT_CONFIG_GLOBAL="$bench_root/gitconfig"
export GIT_CONFIG_NOSYSTEM=1
export GIT_TERMINAL_PROMPT=0
mkdir -p "$HOME" "$XDG_CONFIG_HOME"
printf '[user]\n\tname = Refuge Benchmark\n\temail = benchmark@example.invalid\n' > "$GIT_CONFIG_GLOBAL"
export PATH="$(dirname "$binary"):$PATH"

"$binary" init --repos "$bench_root/repos" --target "$bench_root/target" >/dev/null
for ((repo_index = 1; repo_index <= repo_count; repo_index++)); do
    name="repo-$repo_index"
    "$binary" repo create "$name" >/dev/null
    work="$bench_root/work-$repo_index"
    git init --initial-branch=main "$work" >/dev/null
    git -C "$work" remote add refuge "$bench_root/repos/$name.git"
    if [[ $repo_index -eq 1 ]]; then
        head -c "$lfs_bytes" /dev/zero > "$bench_root/lfs-object"
        oid=$(sha256sum "$bench_root/lfs-object" | cut -d' ' -f1)
        printf 'version https://git-lfs.github.com/spec/v1\noid sha256:%s\nsize %s\n' "$oid" "$lfs_bytes" > "$work/attachment.bin"
        object="$bench_root/repos/$name.git/lfs/objects/${oid:0:2}/${oid:2:2}/$oid"
        mkdir -p "$(dirname "$object")"
        cp "$bench_root/lfs-object" "$object"
    fi
    for ((snapshot = 1; snapshot < snapshot_count; snapshot++)); do
        printf '%s %s\n' "$repo_index" "$snapshot" >> "$work/history.txt"
        git -C "$work" add history.txt
        git -C "$work" commit -m "snapshot $snapshot" >/dev/null
        git -C "$work" push refuge main >/dev/null 2>&1
    done
done

measure() {
    local label=$1
    shift
    local started finished trace git_processes
    trace="$bench_root/$label.trace.json"
    started=$(date +%s%N)
    GIT_TRACE2_EVENT="$trace" "$@" >/dev/null
    finished=$(date +%s%N)
    git_processes=0
    if [[ -f "$trace" ]]; then
        git_processes=$(grep -c '"event":"start"' "$trace" || true)
    fi
    printf '%s,%s,%s\n' "$label" "$((finished - started))" "$git_processes"
}

printf 'operation,nanoseconds,git_processes\n'
measure status_all "$binary" repo status --all
measure snapshot_list "$binary" snapshots list
measure backup_one "$binary" repo backup repo-1
measure verify_one "$binary" snapshots verify repo-1
mv "$bench_root/repos/repo-1.git" "$bench_root/repo-1.saved"
measure restore_one "$binary" restore repo-1
printf 'fixture_repositories,%s\nfixture_snapshots_per_repository,%s\n' \
    "$repo_count" "$snapshot_count"
printf 'fixture_lfs_bytes,%s\n' "$lfs_bytes"
printf 'target_bytes,%s\n' "$(du -sb "$bench_root/target" | cut -f1)"
printf 'target_files,%s\n' "$(find "$bench_root/target" -type f | wc -l)"
"$binary" snapshots usage
