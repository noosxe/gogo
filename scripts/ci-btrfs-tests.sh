#!/usr/bin/env bash
# CI helper for the snapshots-btrfs job (design 20 §10.3): make a 1 GiB btrfs
# image, loop-mount it with sudo (optionally with user_subvol_rm_allowed),
# chown it to the current user and run the btrfs tests unprivileged on it.
# Usage: scripts/ci-btrfs-tests.sh plain|rm-allowed
set -euo pipefail

mode=${1:?usage: ci-btrfs-tests.sh plain|rm-allowed}
img=$(mktemp -u "${RUNNER_TEMP:-/tmp}/gogo-btrfs-XXXXXX.img")
mnt=$(mktemp -d "${RUNNER_TEMP:-/tmp}/gogo-btrfs-mnt-XXXXXX")
opts=loop
extra=()
case $mode in
  plain) ;;
  rm-allowed)
    opts=loop,user_subvol_rm_allowed
    extra=(GOGO_TEST_BTRFS_IOCTL_ALLOWED=1)
    ;;
  *) echo "unknown mode: $mode" >&2; exit 2 ;;
esac

cleanup() {
  sudo umount "$mnt" 2>/dev/null || true
  rm -f "$img"
  rmdir "$mnt" 2>/dev/null || true
}
trap cleanup EXIT

truncate -s 1G "$img"
nix develop -c mkfs.btrfs -q "$img"
sudo mount -o "$opts" "$img" "$mnt"
sudo chown "$(id -u):$(id -g)" "$mnt"
grep " $mnt " /proc/mounts

env GOGO_TEST_BTRFS_DIR="$mnt" GOGO_TEST_BTRFS_REQUIRED=1 "${extra[@]}" \
  nix develop -c cargo nextest run -p gogo-core --test snapshot_btrfs --no-fail-fast
