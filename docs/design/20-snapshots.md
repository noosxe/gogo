# 20 — Snapshots of a built game (#365)

A checkpoint of a built Linux game, its files and its Wine prefix, taken with the
filesystem's own snapshots, so a change gogo makes can be undone: a DLC add (design 19), a
game or DLC update (#360), a rebuild, a DLC removal (#353). Example: the newest build of a
game is buggy, and the user goes back to the snapshot taken before the update.

Scope: the Linux lane (`gogo linux-app`, design 18). The Mac lane has no snapshot backend
here; APFS snapshots are volume-wide and need root, so a Mac game is out of scope.

**Why its own doc and not a section of design 18.** Snapshots cut across design 18 (build,
library, `move`, `apps remove`, the launcher, `doctor`), design 19 (`dlc add` and its
journal), the planned updates (#360), DLC removal (#353) and cloud saves (#275). Design 18
is already about 2700 lines and is the Linux runtime's contract; this is a feature with its
own state, CLI and crash recovery. The places in 18 and 19 that change point here.

## 0. Summary

| Question | Answer |
|---|---|
| Unit of a snapshot | The game's own dir (`games/<gogid>/` or `<library>/<gogid>/`): `game/`, the prefix with the saves, `launch.conf`, `gogo-launch.toml`, `state.toml`, `content.toml`, `launch.sh`, the icon. Plus a copy of the game's L4 override file, taken with it (§3). |
| btrfs | The game dir is its own subvolume, made at build time. Snapshots are read-only subvolumes in `.gogo-snapshots/<gogid>/` beside the game dir. Everything works unprivileged (§2.1). **v1.** |
| ZFS | The game dir must be the mountpoint of its own dataset with `zfs allow` delegation; root sets that up once, and gogo prints the commands. Snapshots are `<dataset>@gogo-<n>`. **After btrfs.** |
| bcachefs | Same model as btrfs, through its own ioctls. Out of mainline since 6.18, so **not implemented until someone asks**; the backend trait leaves room. |
| ext4 and others | No snapshots, and no reflink-copy fallback in v1. The build plan and `snapshot` commands say so plainly. |
| Mechanism | btrfs: gogo's own small ioctl module on `rustix` (already a dependency); no `btrfs-progs`. ZFS: the system's `zfs` CLI. One `SnapshotBackend` trait, with a fake for tests. |
| Metadata | `<data_root>/snapshots/<gogid>/`: one TOML per snapshot, a copy of the L4 file, the per-game setting, the journals (§4). |
| Rollback, btrfs | A writable snapshot of the target, swapped in with `renameat2(RENAME_EXCHANGE)` (atomic), then the old tree is deleted. A snapshot of the state before the rollback is taken first, so a rollback can itself be undone. |
| Rollback, ZFS | `zfs rollback -r`: snapshots newer than the target are destroyed, listed and confirmed first. |
| Saves | Snapshots hold the saves (the settled decision on #365). A rollback lists the save files newer than the snapshot, copies them to a dated dir by default, and asks; `--yes` skips the question (§6). |
| Defaults | On where supported: an initial snapshot after the first build and setup, and an automatic one before each change gogo makes. The last 5 automatic ones are kept; initial and manual snapshots are never pruned. |
| CLI | `gogo linux-app snapshot <gogid> list|create|rollback|delete|auto`, `--no-snapshots` on builds and changes, `--json` everywhere (§8). |

## 1. Terms

- **Game dir**: `GameDirs::for_game(gogid)`, the dir holding `gogo-launch.toml` (design 18
  §6.1). Its parent is `games/` or a library dir.
- **Snapshot**: an immutable, point-in-time copy of the game dir that the filesystem makes,
  plus the metadata and L4 copy gogo stores with it. Numbered per game: 1, 2, 3, …, never
  reused.
- **Kinds**: `initial` (after the first build and setup), `manual` (`snapshot create`),
  `auto` (before a change gogo makes; each records its trigger).
- **Backend**: what the filesystem under the game dir offers: `btrfs`, `zfs`, `bcachefs`
  (later), or `none` with a reason.
- **Autosnapshot**: the per-game setting that makes gogo take `auto` snapshots.

## 2. Per filesystem

gogo picks the backend from the game dir (or, for a new build, from the dir it will be
created in): `statfs(2)`'s `f_type`, then backend-specific checks. Nothing is configured by
hand. `doctor` and the build plan print what was found and why.

### 2.1 btrfs

**What's needed:** the game dir is a subvolume. Nothing else: no mount option, no root, no
quotas, no `btrfs-progs`.

**Detection:** `f_type == BTRFS_SUPER_MAGIC` (`0x9123683E`), and the game dir is a
subvolume root when it is a directory with inode number 256
("A directory representing a subvolume has always inode number 256",
<https://btrfs.readthedocs.io/en/latest/Subvolumes.html>).

**What an unprivileged user can do.** Checked against the kernel source
(`fs/btrfs/ioctl.c`, `fs/btrfs/inode.c`,
<https://github.com/torvalds/linux/tree/master/fs/btrfs>) and confirmed by hand on a
recent kernel, as a normal user, on a btrfs mount without `user_subvol_rm_allowed`:

| Operation | Unprivileged? | Notes |
|---|---|---|
| Create a subvolume (`BTRFS_IOC_SUBVOL_CREATE`) | yes | write and search access on the parent dir; "subvolume creation is not restricted" in `ioctl.c` |
| Snapshot it, read-write or read-only (`BTRFS_IOC_SNAP_CREATE_V2`, `BTRFS_SUBVOL_RDONLY`) | yes | the caller must own the source subvolume (`inode_owner_or_capable`); source and destination on the same filesystem, else `EXDEV` |
| Write into a read-only snapshot | no | `EROFS`, as intended |
| Flip a snapshot's read-only flag (`BTRFS_IOC_SUBVOL_SETFLAGS`) | yes, for its owner | `EPERM` while a `send` uses it |
| Swap two subvolumes (`renameat2(RENAME_EXCHANGE)`) | yes | the kernel explicitly allows exchanging two subvolume roots (`btrfs_rename_exchange`); confirmed by hand |
| Rename or move a subvolume within the filesystem | yes | plain VFS rename rules |
| Delete with `BTRFS_IOC_SNAP_DESTROY_V2` (`btrfs subvolume delete`) | **only with `user_subvol_rm_allowed`** (or `CAP_SYS_ADMIN`) | `EPERM` otherwise, confirmed by hand |
| Delete by emptying it and `rmdir(2)` | yes, since Linux 4.18 | "btrfs: Allow rmdir(2) to delete an empty subvolume" (<https://lkml.iu.edu/hypermail/linux/kernel/1806.0/02095.html>); a read-only snapshot must be flipped to read-write first, or its files can't be unlinked |
| `btrfs subvolume show`, `subvolume list`, `qgroup show` | no | they search the tree roots and need `CAP_SYS_ADMIN`; gogo never needs them |
| `btrfs filesystem du` (FIEMAP per file) | yes | shared/exclusive per extent, see §7.3 |

So gogo deletes a snapshot by trying `SNAP_DESTROY_V2` first (instant where the mount allows
it) and, on `EPERM`, clearing the read-only flag and removing the tree file by file. The
second path is what most users get. It costs a walk of the tree (milliseconds for a game's
few large files, seconds for a prefix with tens of thousands), and the kernel frees the
extents in the background in both cases.

**Snapshots are not recursive.** A nested subvolume shows up in its parent's snapshot as an
empty stub dir (<https://btrfs.readthedocs.io/en/latest/Subvolumes.html>). Two consequences:

- gogo never creates a subvolume inside a game dir, and snapshots live outside it.
- **A game dir that is a subvolume drops out of snapshots of the subvolume around it**:
  snapper or Timeshift snapshots of `/home`, and `btrfs send`-based backups (btrbk) of it,
  see an empty dir where the game was. File-level backups (restic, borg, rsync) are not
  affected. The build plan says this on its `snapshots:` line, and so does this doc's user
  text (decision D1). The linux-e2e harness snapshots its scratch data root, so it
  builds with snapshots off (§10.4).

**Where snapshots live:** `<parent>/.gogo-snapshots/<gogid>/<n>`, where `<parent>` is the
game dir's parent (`games/` or the library dir). Why there:

- A snapshot must be on the same filesystem as its source; the parent is.
- Not inside the game dir: it would nest, and `move`, `apps remove` and every tree walk
  would have to skip it.
- Not in the data root: a library game is usually on another drive.
- `apps list` lists dirs under `games/` that hold `gogo-launch.toml`, and dot-dirs never do;
  the library check (`Library::resolve`) and `move` learn the name so they never treat it as
  a game or a target.

**Game dirs become subvolumes at build time.** The build creates a new game dir (the dir
`GameDirs::for_build` names) with a plain `mkdir`. With snapshots on (the default) and a
btrfs parent, it creates it with `BTRFS_IOC_SUBVOL_CREATE` instead:

- Depot lane: the game dir is created as a subvolume, then `game/` is materialized into it
  as now (FICLONE from the depot cache).
- Installer lane: the staging dir `.build-<slug>-<pid>/` is created as a subvolume and,
  for a new game, renamed whole into place (a subvolume renames like a dir on one
  filesystem). When the game is there already, its prefix is moved out of the staging
  dir as before; a rename can't cross subvolumes, so that is a reflinked copy, and the
  emptied staging subvolume is removed with `rmdir`.
- A rebuild keeps the game dir, so it stays whatever it was. A depot rebuild replaces
  `game/` inside it; an installer rebuild sets the old prefix aside as `prefix.old-<time>`
  inside it, as today.
- `--no-snapshots` makes a plain dir, as today: a user opting out also opts out of the
  subvolume and its effect on their own backups.

**Converting an existing plain game dir** (games built before this, or with
`--no-snapshots`). A file can't be renamed across a subvolume boundary (`EXDEV`), so the
game is copied into a new subvolume with reflinks, which on one btrfs filesystem costs only
metadata. When: only when the user asks for snapshots on that game (`snapshot <gogid> create`
or `auto on`, after a plan and a prompt), never as a side effect of another command. How,
under the same locks and refusals as a rollback (§5.1):

1. Journal `<data_root>/snapshots/<gogid>/convert.json`.
2. Create the subvolume `<parent>/.gogo-convert-<gogid>/`.
3. Copy the game dir into it with `move`'s copy (`macapp/relocate.rs`): symlinks as
   symlinks, modes, mtimes, `user.*` xattrs (Wine's `user.wine.sd`), FICLONE per file.
4. Verify: the same entries, types, sizes, modes, link targets and xattrs on both sides; the
   bytes are compared only for files that were copied rather than cloned (a FICLONE that
   succeeded shares the source's extents, so there is nothing to compare, and comparing
   70 GB would take minutes for nothing). Write the marker `.gogo-converted` into the copy.
5. `renameat2(RENAME_EXCHANGE)` the copy and the game dir: the commit, atomic.
6. Remove the marker, delete the old plain tree (now at `.gogo-convert-<gogid>`), remove the
   journal.

A crash before step 5 leaves the game untouched; the next run (`doctor` points at it)
deletes `.gogo-convert-<gogid>` and starts again. After step 5 the game dir carries the
marker, so the next run knows the swap happened and finishes step 6. The launcher refuses
while the marker is in the game dir (§5.4). A copy that has to fall back to real copies
(FICLONE refused) needs the game's size free, and the plan says so as `move`'s does.

### 2.2 ZFS (OpenZFS on Linux)

**What's needed:** the game dir is the mountpoint of its own dataset, the user owns its root,
and the user has delegated permissions on it. The delegation, run once by root
(<https://openzfs.github.io/openzfs-docs/man/master/8/zfs-allow.8.html>):

```sh
zfs create -o mountpoint=<library>/<gogid> <pool>/<path>/<gogid>
chown "$USER": <library>/<gogid>
zfs allow -u "$USER" snapshot,rollback,destroy,mount,userprop <pool>/<path>/<gogid>
```

`snapshot`, `rollback` and `destroy` each require `mount` to be delegated as well (the
permission table in `zfs-allow(8)`).

**Why gogo can't make the dataset itself:** on Linux, `mount`, `unmount`, `mountpoint`,
`canmount`, `rename` and `share` can't be delegated, because only root can change the mount
namespace (`zfs-allow(8)`). A user with `create` delegated gets a dataset that isn't
mounted. So the dataset is root's to make, before the build:

- The build plan, for a new game whose parent dir is on ZFS, says: `snapshots: unavailable
  (<parent> is ZFS: the game needs its own dataset)` and prints the three commands above
  with the real names filled in (the parent's dataset from `/proc/self/mountinfo`).
- The build accepts a game dir that already exists when it is an empty dataset mountpoint
  owned by the user, so "run these as root, then build" works.
- An existing game in a plain dir on ZFS is not converted by gogo: the steps need a mount
  change. The doc for users gives the manual procedure (root creates the dataset at a
  temporary mountpoint; `gogo linux-app move` copies the game there; root sets the final
  mountpoint). Automating that is not planned.

**Detection:** `f_type == 0x2fc12fc1` (ZFS_SUPER_MAGIC; to verify in the implementation),
the game dir is a mountpoint in `/proc/self/mountinfo` with fstype `zfs`, whose source is the
dataset name. The permissions come from `zfs allow <dataset>` (readable unprivileged), parsed
for the user, their groups and `everyone`. A missing permission is a `doctor` finding naming
the `zfs allow` line to run.

**Snapshots** are `<dataset>@gogo-<n>`, with the user property `gogo:snapshot=<n>` (needs
`userprop`) so a stray `@gogo-*` made by hand isn't mistaken for one of gogo's. A snapshot's
`used` is the space only it holds, and `written` what changed since the one before
(<https://openzfs.github.io/openzfs-docs/man/master/7/zfsprops.7.html>).

**Not yet verified, so the ZFS issue (§12, I9) starts by checking on a real pool:** that a
delegated `zfs rollback` of a mounted dataset works for a non-root user on Linux (it doesn't
call `mount(2)`, so it should), and that `.zfs/snapshot/<name>` automounts for a non-root
reader (§6.1 uses it to compare saves).

### 2.3 bcachefs

bcachefs has btrfs-style writable snapshots, `subvolume create`, `subvolume snapshot [-r]`
and `subvolume delete` (<https://bcachefs.org/Snapshots/>,
<https://man.archlinux.org/man/extra/bcachefs-tools/bcachefs.8.en>). Its design doc says an
untrusted user can create a subvolume where they can `mkdir` and snapshot one whose root
they own. It was removed from the mainline kernel in 6.18 and ships as a DKMS module
(<https://lwn.net/Articles/1040120/>).

**Decision:** the backend trait fits it (the same shape as btrfs: a subvolume per game,
sibling snapshots, an exchange to roll back), but it isn't implemented in this round. Until
then a bcachefs game dir gets `none (bcachefs: not supported yet)`. To check before
implementing: unprivileged delete, `RENAME_EXCHANGE` of two subvolumes, and the statfs magic.

### 2.4 No snapshot support

ext4, XFS, tmpfs, NFS, a FUSE mount, an unmounted library: the backend is `none` with a
reason, and gogo says so where it matters, without failing the command that asked:

- Build plan: `snapshots: unavailable (<dir> is on ext4; snapshots need btrfs, or a ZFS
  dataset per game)`.
- `snapshot <gogid> create|rollback|auto on`: refused with that line, exit 7.
- `snapshot <gogid> list`: `no snapshots: <reason>`, exit 0.
- `apps show`, `doctor`: one `snapshots` line with the reason (`doctor`: `ok`, not a
  warning; a game on ext4 isn't broken).

**No fallback in v1.** A reflink copy (`cp -a --reflink`) on XFS would give a checkpoint
without native snapshots, but it isn't atomic, it walks and duplicates every inode, and a
rollback becomes a tree swap built from copies. The common case the brief names, a data dir
on btrfs and a library on ext4, gets nothing from it either, since ext4 has no reflink. It
stays a possible later backend behind the same trait (decision D9), not part of this
design.

**The common real setup:** data dir on btrfs, library on ext4. Snapshots follow the game
dir, not the data root: a game under `games/` gets them, a game in the ext4 library
doesn't, and `move` between the two says what happens (§9.1).

## 3. What a snapshot covers

Inside the game dir (taken by the filesystem, atomically): `game/`, the prefix (saves,
registry, Proton's prefix upgrades), `launch.conf`, `launch.sh`, `gogo-launch.toml`,
`state.toml`, `content.toml` (the build and DLC set), `icon.png`, `install/`, and leftovers
such as `prefix.old-<time>`. Runtime markers (`.launchers/`, `.one-offs/`) are in it too, and
a rollback clears them, since nothing runs (§5.1).

Outside the game dir, each thing is settled one way:

| What | Where | In a snapshot? | On rollback |
|---|---|---|---|
| L4 override | `<data_root>/compat/<gogid>.toml` | **copied** into the snapshot's metadata dir (or recorded as absent) | restored by default, `key old -> new` lines printed; `--keep-override` keeps the current one. It holds the user's env, args, wrapper, display and pinned runtime/renderer, and its hash is part of the launch stamp, so the restored game and its override match. |
| Location record | `<data_root>/game-locations/<gogid>` | no | untouched: a rollback happens where the game is now |
| Menu entry, launcher | `$XDG_DATA_HOME/applications/gogo-<gogid>.desktop`, `launch.sh` | no (`launch.sh` is inside but rewritten) | rewritten by `launcher::write` from the restored files, as `move` does: always the current `LAUNCHER_VERSION` |
| Logs | `$XDG_STATE_HOME/gogo/logs/<gogid>/` | no | untouched; they're history. The rollback appends a line to `launch.log`. |
| Runtime, renderers | the store (`<data_root>/runtimes/`, `renderers/`) | no, referenced | the store's GC counts every snapshot's runtime and renderers as in use (§9.6), so a rollback never finds its GE-Proton pruned |
| Depot cache | `$XDG_CACHE_HOME/gogo/depot/<gogid>` | no | untouched. It may now hold another build than the game; `gogo list` already shows that (`+ cache (other build)`). |
| Snapshot metadata, autosnapshot setting | `<data_root>/snapshots/<gogid>/` | no | updated by the rollback itself |

Why copy the L4 file rather than move it into the game dir: L4 is the cross-lane override
(`compat/`, design 16) that the store's GC, `compat`, `env`, `args` and the Mac lane read at
that path. Moving it would touch every one of them for one file of a few hundred bytes. A
copy per snapshot costs nothing and makes the rollback consistent.

`launch.conf` in a snapshot names the game dir it was taken in (absolute for a library game).
If the game moved on the same filesystem since (its snapshots move with it, §9.1), the rollback
runs `move`'s `rewrite_inside(from = snapshot's dir, to = current dir)` before publishing.

## 4. Metadata and settings

All under `<data_root>/snapshots/<gogid>/`, written atomically (06 §2):

```
settings.toml          # auto = true, next = 8
3.toml                 # one per snapshot
3.override.toml        # the L4 copy (absent when L4 didn't exist; 3.toml says which)
rollback.json          # journal while a rollback runs (§5)
convert.json           # journal while a conversion runs (§2.1)
```

Why the data root and not beside the snapshots: it is the one place every gogo run can
reach, so `list`, `apps show` and `doctor` still work when a library drive isn't mounted
(they show those snapshots as unreachable). The autosnapshot setting must not live in the
game dir either: a rollback would turn it back.

`<n>.toml`:

```toml
id = 3
created = 2026-11-12T10:41:07Z
kind = "auto"                   # initial | manual | auto
trigger = "dlc-add"             # auto only: dlc-add | update | rebuild | dlc-remove | runtime | rollback | cloud-pull
label = "before adding Phantom Liberty"
state = "done"                  # creating | done | deleting
backend = "btrfs"               # btrfs | zfs
game_dir = "/games/1423049311"  # where the game was when it was taken
override = "copied"             # copied | absent

[content]                       # from content.toml and state.toml at that moment
lane = "depot"
build_id = "58424385209538727"
version = "2.31"
language = "en-US"
dlcs = [1256837418]
runtime = "GE-Proton10-25"
renderer = "dxvk"

[space]                         # last measured, §7.3
checked = 2026-11-12T11:02:00Z
exclusive_bytes = 912345678
exact = false
```

Labels are gogo's for `auto` and `initial` (`after the first build`, `before adding
Phantom Liberty`, `before update 2.31 → 2.5`, `before rebuild`, `before removing Phantom
Liberty`, `before GE-Proton10-25 → GE-Proton10-30`, `before rollback to 3`, `before cloud-save
download`), and the user's for `manual` (`--label`, else `manual`).

`settings.toml`'s `auto` is set at the first build: `true` unless `--no-snapshots` or the
backend is `none`. A game built before this has no file and counts as `auto = false` (§9.7).
`next` is the next snapshot number.

The snapshot's on-disk name is the number: `.gogo-snapshots/<gogid>/3` on btrfs,
`<dataset>@gogo-3` on ZFS. A snapshot found on disk with no metadata (a data root wiped or
restored from a backup) is listed as `?` with its date from the filesystem, and can be
deleted; one in the metadata and not on disk is listed as `missing` and `doctor` warns.

## 5. Operations and crash safety

### 5.1 Locks and refusals (every operation that changes a game)

`create`, `rollback`, `delete`, a conversion and turning autosnapshot on take the same locks a
`move` takes, in the same order: the data root's lock, then the game's
`game-moves/<gogid>.lock`, exclusively, for the whole run. The launcher (v8+) already refuses
to start while the game's lock is held. They refuse, with exit 7 and nothing touched:

- while the game runs: `refuse_running` (a live `.launchers/<pid>`, any process whose
  environment names the prefix) and a live `.one-offs/<pid>` (a `linux-app run` of another
  exe, design 18 §7.10);
- while a DLC lay is pending (`dlc-journal.json`, `dlc-download.json` or `.dlc-staging/` in
  the game dir): "finish it first: `gogo linux-app dlc <gogid> add`" (design 19 §3.4). A
  snapshot of a half-laid tree would be a checkpoint nobody wants, and a rollback under a
  journal would make its recovery undo the wrong files;
- while a move is unsettled (`game-moves/<gogid>` exists);
- while another snapshot journal is pending: it is settled first (§5.3), as `move` settles an
  interrupted move before anything else;
- for an unreachable game (its drive isn't mounted).

An `auto` snapshot is taken by the command that is about to change the game, under the locks
that command already holds, after its own refusals and before its first write.

A snapshot of a running game would be crash-consistent at best (a save half written), so
`create` refuses too, rather than offering a flag (decision D10).

### 5.2 Create, delete

**Create (btrfs):** `<n>.toml` with `state = "creating"` → the L4 copy → `SNAP_CREATE_V2`
read-only into `.gogo-snapshots/<gogid>/<n>` → `state = "done"` → retention (§7.2).
**ZFS:** the same, with `zfs snapshot <ds>@gogo-<n>` and `zfs set gogo:snapshot=<n>`.
Recovery of a `creating` entry: the snapshot exists → finish (the L4 copy was taken before
it); it doesn't → drop the entry.

**Delete:** `state = "deleting"` → btrfs: `SNAP_DESTROY_V2`, else clear read-only and remove
the tree (§2.1); ZFS: `zfs destroy <ds>@gogo-<n>` → drop the entry and its L4 copy. Recovery
of a `deleting` entry: carry on.

### 5.3 Rollback

**Plan and prompt.** The rollback prints what it will do before doing anything: the target
(number, date, label, build and DLC set) against what the game holds now, the override lines
that change, the save files newer than the snapshot (§6), and on ZFS the snapshots it will
destroy (§5.3.2). On a TTY it asks; with `--yes` it doesn't; without a TTY and without
`--yes` it prints the plan and exits 2 (03 §3). `--dry-run` prints the plan and stops.

#### 5.3.1 btrfs

1. Journal `rollback.json`: `{to, pre, staging}`.
2. **The pre-rollback snapshot:** an `auto` snapshot (`trigger = "rollback"`, `before
   rollback to <n>`), taken whether autosnapshot is on or not. It makes the rollback
   undoable and holds the newer saves; it costs only metadata and is pruned like any `auto`
   snapshot (decision D3).
3. The copy-aside of newer saves (§6.2), unless `--drop-saves`.
4. A writable snapshot of `<n>` at `<parent>/.gogo-rollback-<gogid>`, with the marker
   `.gogo-rollback` written into it (the target's number).
5. **The commit:** `renameat2(<parent>/.gogo-rollback-<gogid>, <game dir>, RENAME_EXCHANGE)`.
   One atomic step; there is never a moment without a game dir.
6. `rewrite_inside` if the game dir changed since the snapshot (§3); the L4 copy restored
   (unless `--keep-override`); `launcher::write` (launch.sh, icon, menu entry); clear
   `.launchers/` and `.one-offs/` leftovers; remove the marker.
7. Delete the old tree, now at `.gogo-rollback-<gogid>` (as a snapshot is deleted, §2.1).
8. Remove the journal.

No setup runs: the restored game carries the `state.toml` and stamp setup wrote for it, and
its override is the one that matched. If the user kept a newer override (`--keep-override`),
the stamp differs and the launcher runs setup at the next launch, as after any `env set`.

**Interrupted:**

| Found (journal present) | What happens |
|---|---|
| no `.gogo-rollback-<gogid>`, game dir without the marker | before step 4: the game is as it was. Remove the journal; the pre-rollback snapshot, if taken, stays as an ordinary `auto` one. |
| `.gogo-rollback-<gogid>` with the marker, game dir without it | before the commit: delete the staging tree, remove the journal. The game is as it was. |
| game dir with the marker | after the commit: finish steps 6–8. |
| no marker anywhere, `.gogo-rollback-<gogid>` without one | after step 6, the old tree left: delete it, remove the journal. |

The next `snapshot` command, `dlc add`, `move`, `apps remove` and a build settle it first;
`doctor` reports it (`fail`, with the command to run). The launcher refuses while
`.gogo-rollback` is in the game dir (§5.4), so a half-finished rollback never runs with a
mismatched override.

#### 5.3.2 ZFS

`zfs rollback` only goes back to the newest snapshot; `-r` destroys the newer ones first
(<https://openzfs.github.io/openzfs-docs/man/master/8/zfs-rollback.8.html>). Keeping them would
take `zfs clone` plus `zfs promote` plus a rename or mountpoint change, and on Linux the last
two are root-only (`zfs-allow(8)`). So:

- Rolling back to `<n>` destroys every snapshot newer than `<n>`, initial and manual ones too.
  The plan lists them with their labels and sizes, and the prompt names the count. That is
  the price of ZFS without root, and the reason btrfs is first.
- No pre-rollback snapshot: `-r` would destroy it at once. The copy-aside of newer saves (§6)
  is all that keeps them.
- Steps: journal → copy-aside → `zfs rollback -r <ds>@gogo-<n>` (the commit, atomic in ZFS)
  → step 6 as on btrfs → drop the metadata of the destroyed snapshots → remove the journal.
- Interrupted: a journal and snapshots newer than `<n>` still present → nothing happened,
  remove the journal; none left → after the commit, finish.
- A copy-back from `.zfs/snapshot/gogo-<n>/` would keep the newer snapshots, but it is a
  file-by-file copy over a live tree: slow, not atomic, and an interrupted one leaves a mix.
  Not chosen (decision D7).

### 5.4 The launcher

`LAUNCHER_VERSION` goes up by one. The new launcher refuses, with the same dialog and log
footer as its other refusals, while the game dir holds `.gogo-rollback` or `.gogo-converted`
(a rollback or conversion stopped after its commit). The locks already keep it from starting
while one runs. An older launcher doesn't check, which only matters in a crash window; the
next setup replaces it.

## 6. Saves on rollback

The decision on #365: snapshots include the saves, and the prefix is restored whole. What a
rollback drops is saves made after the snapshot. It lists them and asks before dropping them,
`--yes` skips the question, and a copy aside is an option. This section makes that concrete.

### 6.1 Finding the newer saves

The **save roots**, relative to the prefix's `drive_c/users/steamuser/` (Proton's user):
`Documents/`, `Saved Games/`, `AppData/Roaming/`, `AppData/Local/`, `AppData/LocalLow/`;
plus `drive_c/ProgramData/`; plus **the game's own files** (below); plus, once cloud saves
(#275) land, the game's own cloud-save locations from GOG's metadata.

**The game's files are a save root too.** Many older games keep saves and settings in their
own install dir, beside the exe: Heroes of Might and Magic 3 writes `game/Games/*.GM1`, the
SimCity and Anno 1503/1701-era games write `SAVE/` dirs and `*.ini` files there. A rollback
restores those files whole, so they are scanned like the prefix's user dirs. The game's files
are `game/` on the depot lane and the install dir `drive_c/GOG/<slug>/` on the installer lane.
There, a file is a save **candidate** by the same two tests (mtime later than the snapshot's
`created`, or absent from the snapshot), and then the files that **belong to the build at
that path** are taken out, using the current build's file list (the game as it is now is what
the rollback replaces):

- **Depot lane:** the build's depot manifests for the build, language and DLC set that
  `content.toml` records (`.gogo-depot.json` names the same for the cache). gogo reads them
  from the depot cache when the cache holds the game's build, else fetches them as
  `depot::installed_build` does (design 19 §3.2). A path the manifests list is the build's
  when its size is the listed size. A listed file whose size differs (a shipped `.ini` the
  game rewrote) stays a candidate. A same-size rewrite of a shipped file is missed: the
  snapshot still holds it on btrfs, and the plan says the check is by name and size.
- **Installer lane:** the files the installers laid, from their Inno `/LOG` logs
  (`install/setup.log`, design 18 §10, and the DLC installers' logs where gogo keeps them),
  which name each destination file. A path the logs list is the build's, by the same size
  test. Parsing those logs on real installers is checked when the finder is implemented
  (I4).
- **Files gogo itself writes there** are never candidates: the compat `files` (design 18
  §4.10), renderer DLLs and configs setup places beside the exe (cnc-ddraw's `ddraw.dll` and
  `ddraw.ini`, the D7VK/DXVK DLLs), and the support overlays.
- **No list** (the manifests can't be read or fetched offline, a build GOG no longer serves
  and no cache, an installer log missing or unreadable): the mtime test alone, on files that
  also exist in the snapshot (a file the game rewrote in place), with the same exclusions.
  New files can't be told apart from what a DLC add or update laid after the snapshot, so
  when the build or DLC set changed since the snapshot they are shown as one summary line
  (`312 new files in game/ gogo can't attribute: not copied`; on btrfs, `they stay in
  snapshot <pre>`), every one with `-v` and in `--json`. When the build and DLC set are the
  same as the snapshot's, nothing gogo laid is new, so new files are candidates as usual.
  The plan says which way it checked (`game files: checked against the build's file list`,
  or `game files: no file list, changed files only`).

A file is **newer** when it is under a save root in the current game dir and either its
mtime is later than the snapshot's `created`, or it doesn't exist in the snapshot. gogo
reads the snapshot's tree for the second test: on btrfs it is a dir; on ZFS
`.zfs/snapshot/gogo-<n>/` (if that doesn't automount for the user, the mtime test alone).
Files that only differ in the other direction (deleted since, or older) are not saves at
risk: the rollback brings them back.

**Excluded** (caches and logs a game, Wine or a driver rewrites at every start, which would
otherwise drown the real saves): `AppData/Local/Temp/`, `AppData/Local/Microsoft/`,
`*/INetCache/`, `AppData/Local/D3DSCache/`, `AppData/Local/NVIDIA*/`, `*.dxvk-cache`,
`vkd3d-proton.cache*`, `CrashDumps/`, `*.log`, `*.dmp`. In the game's files the same
caches and logs are excluded, plus the cache files Wine, DXVK and VKD3D-Proton write beside
the exe (`*.dxvk-cache`, `*.dxvk-state-cache`, `vkd3d-proton.cache*`, `dxgi.log`-style logs,
covered by `*.log`) and Windows' thumbnail cache (`Thumbs.db`). The list is a constant in code,
covered by a test, and grows as games show more; settings files (`*.ini`, `*.cfg`) are not
excluded, because losing them is a loss too.

**Shown** grouped by dir with count, size and newest mtime (`Saved Games/CD Projekt Red/
Cyberpunk 2077: 14 files, 212 MiB, newest 2026-11-12 09:58`); every file with `-v` and in
`--json`.

### 6.2 What happens to them

- **Copied aside by default:** to `<data_root>/saves-aside/<gogid>/<UTC timestamp>/`, the
  paths kept relative to `drive_c/` (files from `game/` under `game/`), with FICLONE where
  the filesystem allows. Saves are
  small next to a game, and "bytes are sacred": the default keeps them. `--drop-saves` skips
  the copy. The rollback's last line names the dir. `saves-aside/` is the user's: gogo never
  prunes it, `apps remove` lists it and asks before removing it with the game, and `apps
  show` and `doctor` name it with its size (decision D2).
- **On btrfs** the pre-rollback snapshot holds them as well, so a rollback can be undone whole
  with `snapshot <gogid> rollback <pre>`.
- **The question** (TTY, no `--yes`): `roll back to 3 (before adding Phantom Liberty)? 14
  newer save files will be dropped from the game (copied to <dir>) [y/N]`. With
  `--drop-saves`: `… dropped from the game (kept in snapshot 8 only) [y/N]` on btrfs and
  `… dropped (no copy) [y/N]` on ZFS.
- With no newer saves, the question is the plain `roll back to 3 (…)? [y/N]`.

Putting the saves back into the rolled-back game is not done automatically: a newer
version's saves often don't load in the older one, which is why the rollback is wanted. The
user copies what they want from the aside dir.

### 6.3 Cloud saves (#275)

#275 has no design yet. These are the constraints this design puts on it:

1. **A rollback makes local saves older than the cloud's.** The next sync must not silently
   pull the newer cloud saves back (undoing the rollback) nor push the rolled-back ones over
   them. A rollback records `rolled_back = <time>` in the game's cloud-save state; the next
   sync treats every file that differs as a conflict and asks (`--prefer local|cloud`),
   never resolving it alone, and clears the mark once settled.
2. **A pull that overwrites local saves is a change to the game:** with autosnapshot on, gogo
   takes an `auto` snapshot (`trigger = "cloud-pull"`, `before cloud-save download`) before
   writing. A sync that only adds files, or only pushes, doesn't.
3. **The save roots come from #275 once it lands:** GOG's declared save locations are a
   better list than §6.1's guess, which stays as the fallback for games without cloud saves.
   Those locations may point into the game's files (`game/` or the install dir); the
   build-file filter of §6.1 still applies there, so a declared save dir that also holds
   shipped files lists only what the game wrote.
4. Snapshots never touch the cloud. A rollback doesn't push, and deleting a snapshot doesn't
   delete anything remote.

## 7. Autosnapshot, retention, space

### 7.1 When gogo takes one

| Moment | Kind, trigger | Label |
|---|---|---|
| A first build, after its setup succeeded | `initial` | `after the first build` |
| `dlc add`, after its recovery and refusals, before its journal | `auto`, `dlc-add` | `before adding <title>[, <title>…]` |
| An update (#360), before its journal | `auto`, `update` | `before update <old> → <new>` |
| A rebuild of a built game | `auto`, `rebuild` | `before rebuild` |
| A DLC removal (#353) | `auto`, `dlc-remove` | `before removing <title>` |
| A setup that changes the game's GE-Proton (`--retarget`, a new pinned runtime), since Proton upgrades the prefix in place and can't go back | `auto`, `runtime` | `before GE-Proton10-25 → GE-Proton10-30` |
| A rollback (btrfs) | `auto`, `rollback` | `before rollback to <n>` (always, §5.3.1) |
| A cloud-save pull that overwrites local files (#275) | `auto`, `cloud-pull` | `before cloud-save download` |
| After a `move` to another filesystem (the old snapshots are gone, §9.1) | `initial` | `after moving to <dir>` |

Not a trigger: `env`, `args`, `wrapper`, `display`, `next-renderer` (they edit the L4 file,
which is restored with any snapshot anyway), a `move` on one filesystem, `linux-app run`,
and the launcher's own setup when only the stamp changed.

An autosnapshot that fails (a full disk, a delegation gone missing on ZFS) stops the change
it was protecting, with exit 7 and a hint: `--no-snapshots` on that command skips it for that
run. A protected change never goes ahead silently unprotected.

`--no-snapshots` on `dlc add`, an update and a rebuild skips that run's snapshot and leaves
the setting alone. On a first build it skips the initial snapshot, sets `auto = false` and
makes a plain game dir (§2.1).

### 7.2 Retention

- `initial` and `manual` snapshots are never pruned. They go only by `snapshot delete` or
  `apps remove`.
- `auto` snapshots: the newest `snapshots.keep_auto` (default **5**, config 05) are kept per
  game; after a new `auto` snapshot is done, older ones beyond that are deleted, oldest first,
  each reported (`pruned snapshot 2 (before rebuild, 2026-10-30)`).
- No pruning by age or space in v1: CoW snapshots of a game cost what changed, and the
  space is shown (§7.3). A space-based rule can come later (D6 keeps v1 without one).
- `keep_auto = 0` keeps none: autosnapshot then only protects a change until the next one.

### 7.3 Space

What a snapshot costs is the data only it holds:

- **ZFS:** exact, the snapshot's `used` property.
- **btrfs with quotas:** qgroups would give exact exclusive sizes, but reading or enabling
  them needs `CAP_SYS_ADMIN` and quotas are usually off
  (<https://btrfs.readthedocs.io/en/latest/Qgroups.html>). gogo doesn't use them.
- **btrfs without quotas (what gogo does):** a FIEMAP walk, unprivileged, the method of
  `btrfs filesystem du` (<https://btrfs.readthedocs.io/en/latest/btrfs-filesystem.html>).
  gogo collects the physical byte ranges the files of the game dir and of each of its
  snapshots use, and counts, per snapshot, the bytes of ranges no other tree in that set
  covers. Ranges rather than whole extents: btrfs reports the part of an extent a file
  references, so the old part of an extent a later write only partly replaced counts as the
  snapshot's (`btrfs filesystem du` compares whole extents, and a hand test saw it report
  0 B exclusive for a snapshot holding 2 MiB of overwritten data). It is still an
  **estimate**, shown with `~`: btrfs keeps a partly replaced extent whole until nothing
  references any of it, compressed extents are counted by their logical length, and data
  shared with something outside the set (a reflink copy elsewhere) counts as exclusive. It
  is also not "what deleting it frees" when two snapshots share data that the game dir no
  longer has.
- Measured on `snapshot list` (skipped with `--no-space`) and stored in `<n>.toml` with its
  date; `apps show` and `doctor` show the stored values with their date rather than walking.
  The walk is fast for a game's few large files and takes seconds for a large prefix.
- Also shown: the game dir's size, and the filesystem's free space.

## 8. CLI

The shape follows `dlc <gogid> list|add`: the noun, the gogid, then the verb (design 18
§7.10's naming rule). Everything takes `--json`; changes take `--yes` and `--dry-run`; refusals
exit 7, a bad id or flag exits 2, and a non-TTY without `--yes` prints the plan and exits 2
(03 §3, §5).

```
gogo linux-app snapshot <gogid> list [--no-space] [--json]
gogo linux-app snapshot <gogid> create [--label <text>] [--yes] [--dry-run] [--json]
gogo linux-app snapshot <gogid> rollback <n> [--drop-saves] [--keep-override] [--yes] [--dry-run] [--json]
gogo linux-app snapshot <gogid> delete <n>… [--yes] [--dry-run] [--json]
gogo linux-app snapshot <gogid> auto [on|off] [--yes] [--json]

gogo linux-app <game> … --no-snapshots
gogo linux-app dlc <gogid> add … --no-snapshots      # and the update and rebuild paths
```

- **`list`:** the backend line (`btrfs subvolume`, `zfs <dataset>`, or `none: <reason>`),
  `autosnapshot: on, keeping 5 automatic`, then one row per snapshot, newest first: `N  DATE
  KIND  LABEL  BUILD  DLC  SPACE`. `BUILD` is the version (or the build id's tail on a depot
  build without one), `DLC` the count, `SPACE` `~912 MiB` (btrfs) or `912 MiB` (ZFS).
- **`create`:** takes a `manual` snapshot. On a plain btrfs game dir it converts it first
  (§2.1), after the plan and the question; `--yes` agrees.
- **`rollback <n>`:** §5.3 and §6. Prints the done steps and, last, `rolled back to 3; the
  state before is snapshot 8; newer saves copied to <dir>`.
- **`delete <n>…`:** asks (`delete snapshot 1 (after the first build), ~4.1 GiB?`); an
  `initial` one is named as such in the question. `auto` snapshots are deleted the same way.
- **`auto`:** with no argument, prints the setting. `on` converts a plain btrfs dir first
  (as `create`) and takes no snapshot by itself; `off` keeps existing snapshots.
- `--json` for `list`:

```json
{ "gogid": 1423049311, "backend": "btrfs", "reason": null,
  "auto": true, "keepAuto": 5, "freeBytes": 412000000000,
  "snapshots": [
    { "id": 3, "created": "2026-11-12T10:41:07Z", "kind": "auto", "trigger": "dlc-add",
      "label": "before adding Phantom Liberty", "state": "done", "reachable": true,
      "content": { "lane": "depot", "buildId": "58424385209538727", "version": "2.31",
                   "language": "en-US", "dlcs": [1256837418], "runtime": "GE-Proton10-25",
                   "renderer": "dxvk" },
      "space": { "exclusiveBytes": 912345678, "exact": false, "checked": "2026-11-12T11:02:00Z" } } ] }
```

- `rollback --json` reports `{to, pre, savesNewer: [{path, size, mtime}], savesCopiedTo,
  overrideRestored, destroyed: [ids]}`; `--json-progress` emits `SnapshotCreated`,
  `SnapshotPruned`, `SnapshotDeleted`, `RolledBack`, `SnapshotRecovered`.
- Build plan line: `snapshots: on (btrfs subvolume; first snapshot after setup; the game
  dir leaves snapshots of the subvolume around it)`, `snapshots: off (--no-snapshots)`, or
  `snapshots: unavailable (<reason>)`.
- Config (design 05): `[snapshots] auto = true` (the default for new builds) and
  `keep_auto = 5`.

### 8.1 As built (#403)

What the first implementation settled where this design left room:

- **Questions.** `delete` asks (TTY), `--yes` skips it, and without a TTY it prints the
  plan and exits 2. `create` and `auto on|off` ask nothing until they convert a plain dir
  (#406); until then they refuse it, exit 7: `<dir> isn't a btrfs subvolume …: it needs
  converting to one first`. `--yes` is accepted on both for that.
- **Refusals** are checked twice: once before anything is shown or asked (exit 7, nothing
  touched), and again under the two locks, after an interrupted create or delete is
  settled. `auto off` changes nothing about the game, so it takes the locks but, of §5.1's
  refusals, only the unreachable game's. A snapshot number that isn't the game's is a usage error (exit 2).
- **The build.** A subvolume that can't be made becomes a plain dir with a warning, and
  the build goes on without snapshots. The `initial` snapshot is taken after setup and
  after `content.toml` is written, so it holds both; if it fails, the build still
  succeeds and warns (the change it would protect is already done; §7.1's stop applies to
  `auto` snapshots). `settings.toml` is written at every first build: `auto = true` only
  when the game dir is a subvolume.
- **The plan line on a rebuild:** `on (a rebuild: the game dir stays the subvolume it is)`
  or `off for this game (a rebuild: the game dir stays as it is)`, from the game's setting
  and its dir. `[snapshots] auto = false` shows as `off ([snapshots] auto = false)`.
- **`[content]`'s `runtime` and `renderer`** are `state.toml`'s store references
  (`ge-proton/GE-Proton10-25`, `dxvk/<ver>`), so the store's GC and `doctor` match them
  as they match a game's.
- **`list --json`** also carries `ready` (the game dir is a subvolume), `gameDir`,
  `reachable`, each snapshot's `presence` (`present`, `missing`, `stray`, `unreachable`)
  and `notes` (a space measurement that couldn't run: the locks were held). The space is
  measured under the locks; a game that is running doesn't stop `list`.
- **`.gogo-snapshots`:** `apps list` only takes dirs named by a gogid, so it never sees
  it. `Library::resolve` refuses any path with a `.gogo-snapshots` part.

`apps show` gains a `snapshots` section: backend, autosnapshot, count, the newest three with
labels, the stored space total with its date, and the saves-aside dirs. `doctor` gains a
`snapshots` check:

| Finding | Level |
|---|---|
| backend `none` with its reason; count and space when there are snapshots | ok |
| autosnapshot on, but the game dir is no longer a subvolume or dataset (copied by hand) | warn: snapshots stopped; `snapshot <gogid> auto on` converts it |
| a game built before snapshots, on a backend that has them | ok, with the hint `snapshot <gogid> auto on` |
| ZFS: a permission missing from `zfs allow` | warn, naming the `zfs allow` line |
| a snapshot in the metadata missing on disk, or one on disk without metadata | warn |
| a snapshot's runtime missing from the store | warn: a rollback to it needs that GE-Proton |
| a pending `rollback.json` or `convert.json`, a marker in the game dir, a leftover `.gogo-rollback-<gogid>` / `.gogo-convert-<gogid>` | fail, naming the command that settles it |

## 9. Interplay

### 9.1 `move` (design 18 §6.1)

- **Same filesystem (a rename):** the game's `.gogo-snapshots/<gogid>/` is renamed to the
  target parent's `.gogo-snapshots/` in the same journaled run, after the data and before the
  commit (a subvolume renames like a dir; a ZFS dataset's snapshots go with it). Recovery
  learns the extra step. `<n>.toml`'s `game_dir` stays the old path: the rollback rewrites
  `launch.conf` from it (§3).
- **Across filesystems:** snapshots can't move (a snapshot only exists on its own
  filesystem), and copying each one would cost the game's size per snapshot. **They are
  deleted after the move commits**, with the metadata. The plan lists them with their space;
  `move` then needs a yes (TTY question, or `--yes`; a non-TTY without it prints the plan and
  exits 2). `move` gains `--yes` for this. Keeping them behind on the old drive was
  considered and rejected: a rollback can't cross filesystems, so they would be orphans
  holding space (decision D5).
- **The target's backend:** btrfs → the copy target `.moving-<gogid>/` is created as a
  subvolume, and with autosnapshot on an `initial` snapshot (`after moving to <dir>`) is
  taken after the commit. ZFS → refused unless the target is a prepared dataset (§2.2); a
  move into a library on ZFS without one gets a plain dir and `none`. `none` → the setting
  stays as it was and does nothing there, and `list` says why.
- `--adopt` moves no data, and the snapshots stay beside the dir they were taken next to;
  if that's no longer the game dir's parent, `doctor` reports them as unreachable and
  `snapshot delete` can remove their metadata.

### 9.2 Libraries (#279)

Snapshots follow the game dir's filesystem, never the library setting. The snapshots of a
game on a library drive that isn't mounted are listed as unreachable, and every change is
refused as for that game (design 18 §6.1, "A drive that isn't there").

### 9.3 `apps remove`, `apps prune`

The prompt adds the snapshots (`and 4 snapshots, ~6.2 GiB`) and the saves-aside dirs (asked
separately, default no). Snapshots are deleted before the game dir; the metadata dir goes
last. ZFS: the snapshots are destroyed, the dataset is emptied, and the dataset itself stays
(destroying a mounted dataset needs root): the report says so and prints the `zfs destroy`
line. An unreachable game is forgotten with its metadata, and the report says the snapshots
are still on that drive, as for its files.

### 9.4 DLC add and its journal (design 19 §3.4)

`dlc add` recovers an interrupted lay first, then refuses as today, then takes its `auto`
snapshot, then writes its journal. No snapshot operation runs while a journal is pending
(§5.1). Rolling back to before a DLC add is the way to undo it until #353 exists. After a
rollback to an older build, a later `dlc add` needs that build's files; GOG may no longer
list it, and design 19's refusal already says so.

### 9.5 Updates (#360), DLC removal (#353), rebuilds

- An update takes `before update <old> → <new>` before its journal. A rollback restores
  `content.toml`, so the game shows the update as available again.
- DLC removal takes `before removing <title>`. Until it exists, a rebuild with a smaller
  `--dlc` set is the workaround, and it takes `before rebuild`.
- A rebuild takes `before rebuild`. The installer lane's `prefix.old-<time>` set-aside stays
  as it is; whether snapshots make it redundant on btrfs is a follow-up, not part of this.

### 9.6 The store's GC

`runtimes prune` and the GC's reference list (design 16 decision 6, design 18 §6) gain each
snapshot's `content.runtime` and renderers. A snapshot that names a runtime the user removed
by hand is a `doctor` warning, and its rollback says that the next launch's setup fetches it.

### 9.7 Games built before snapshots

They keep working as they are: `auto = false`, a plain dir, no snapshots. `doctor` and `apps
show` mention `snapshot <gogid> auto on` where the backend has snapshots. Nothing converts
on its own (decision D8).

## 10. Implementation and testing

### 10.1 Mechanism: ioctls for btrfs, the CLI for ZFS

| | btrfs ioctls (own module) | `btrfs-progs` CLI | a crate (`btrfs-uapi`, `libbtrfsutil`) | `zfs` CLI | libzfs |
|---|---|---|---|---|---|
| Dependencies | `rustix` (already a direct Linux dependency, for FICLONE) | a host binary, often not installed | `btrfs-uapi` 0.x is MIT/Apache and young; `libbtrfsutil` binds the LGPL C library | the host's `zfs`, there wherever ZFS is | CDDL, no stable ABI, must match the kernel module |
| Privileges | exactly the kernel's (§2.1) | the same, plus `subvolume delete/show/list` that need root | the same | the delegated set | the same |
| Testability | behind the trait; real-fs tests (§10.3) | output parsing | as ioctls | behind the trait; real-pool tests in CI if available | — |

**Decision:** btrfs through a small `snapshot/btrfs.rs` on `rustix::ioctl` (`SUBVOL_CREATE`,
`SNAP_CREATE_V2`, `SUBVOL_GETFLAGS`/`SETFLAGS`, `SNAP_DESTROY_V2`, FIEMAP), `rustix`'s
`renameat_with(RenameFlags::EXCHANGE)` and `statfs`. Five ioctls with fixed structs are less
than a dependency, and the e2e helper already uses two of them (`scripts/linux-e2e-helper.py`,
"btrfs subvolumes (#255)"). The `unsafe` stays in that file. `btrfs-uapi` is the fallback if
the module grows (decision D13). ZFS through the `zfs` CLI with `-H -p` output.

**Finding `zfs` under Nix:** `[linux] zfs = "<path>"` in the config, else the first `zfs` on
`PATH` (on NixOS the system profile's, `/run/current-system/sw/bin`). It is never added to the
flake or the `gogo-linux-app` wrapper: the userland must match the running kernel module, so
only the system's is right. Not found → the backend is `none (zfs: the zfs command isn't on
PATH)`.

**Code shape:** `gogo-core/src/snapshot/` with `mod.rs` (metadata, retention, journals,
saves, the operations), `backend.rs` (the trait), `btrfs.rs`, `zfs.rs`, `fake.rs`
(`#[cfg(test)]` and the test-util feature). The trait:

```rust
pub trait SnapshotBackend: Send + Sync {
    fn probe(&self, dir: &Path) -> Support;                 // Btrfs | Zfs{dataset} | None(reason)
    fn kind(&self) -> BackendKind;                          // `backend` in <n>.toml
    fn ready(&self, game_dir: &Path) -> io::Result<bool>;   // its own subvolume / dataset (else: convert)
    fn make_game_dir(&self, dir: &Path) -> io::Result<()>;  // subvolume; ZFS: check the prepared dataset
    fn snapshot(&self, game_dir: &Path, n: u32) -> io::Result<()>;
    fn snapshot_path(&self, game_dir: &Path, n: u32) -> Option<PathBuf>; // a readable tree, for §6.1
    fn exists(&self, game_dir: &Path, n: u32) -> io::Result<bool>;       // for recovery (§5.2)
    fn delete(&self, game_dir: &Path, n: u32) -> io::Result<()>;          // gone already: Ok
    fn rollback(&self, game_dir: &Path, n: u32, step: &mut dyn FnMut(RollbackStep) -> io::Result<()>) -> io::Result<()>;
    fn on_disk(&self, game_dir: &Path) -> io::Result<Vec<OnDisk>>;        // NotFound: unreachable
    fn space(&self, game_dir: &Path, ns: &[u32]) -> io::Result<Vec<Space>>;
}
```

`rollback` calls back at each step so the orchestration (journal, saves, override, launcher)
stays in `mod.rs` and is the same for every backend. `probe` is about the filesystem and
`ready` about the one game dir: a plain dir on btrfs is `Support::Btrfs` but not ready,
which is the "needs converting" case of §2.1. `exists` answers recovery's one question
without listing every snapshot.

The operations in `mod.rs` (`Store::create`, `delete`, `recover`, `prune`, `measure_space`,
`set_auto`) take a `Guard` built from the two lock files of §5.1, so a caller can't reach
them without having taken the locks; the refusals of §5.1 stay the caller's. `create` moves
`settings.toml`'s `next` past the new number before anything else, so a number is never
handed out twice, even after a crash; it never takes a number that is in the metadata or on
disk (a `?` snapshot keeps its number). Retention is the caller's step after an `auto`
snapshot (`prune(keep_auto)`), not part of `create`.

### 10.2 Unit and integration tests (any filesystem, CI on Linux and macOS for the shared code)

The fake backend keeps "snapshots" as plain copies in a temp dir and implements the exchange
with two renames, with a `fail_at(step)` hook (as `dlc add`'s `AddOptions::fail_at`). With it:

- metadata round-trips, numbering, labels per trigger, retention (`keep_auto`, never pruning
  `initial`/`manual`), the on-disk vs metadata reconciliation;
- every refusal of §5.1 (running, one-off, DLC journal, move journal, unreachable);
- the newer-saves finder: save roots, exclusions, mtime and absent-in-snapshot, grouping;
- saves in the game's files: a fixture `game/` with shipped files from a fake depot manifest
  (and a fake Inno log for the installer lane), a newer `SAVE/slot1.sav`, a rewritten shipped
  `.ini` with a new size, files a later DLC add laid, gogo's own compat and renderer files,
  and `*.dxvk-cache` beside the exe: only the save and the `.ini` are listed. The same tree
  without a list: only files rewritten in place are listed, and the new ones are the summary
  line when the DLC set changed;
- rollback: L4 restored or kept, `rewrite_inside` after a same-fs move, `launcher::write`,
  saves-aside layout, the plan and `--json` shapes, the non-TTY exit 2;
- **crash injection at every step** of create, delete, rollback and convert, then the
  recovery, asserting the game is either the old or the new one and runs (the real
  `launch.sh` against a fake umu, as `linuxapp_pipeline.rs` does);
- `move`, `apps remove`, `dlc add` and the build calling the backend at the right moment
  (`initial` after setup, `--no-snapshots`, the failed-autosnapshot stop).

### 10.3 Real btrfs

- **Locally, no root:** tests in `CARGO_TARGET_TMPDIR` that run when it is on btrfs and
  otherwise print a `SKIP …` line, as the FICLONE tests do (`.config/nextest.toml` shows it).
  They create, snapshot, exchange, delete (through the `EPERM` path when the mount lacks
  `user_subvol_rm_allowed`), convert and roll back a small game tree, and check inode 256,
  read-only behaviour and the FIEMAP estimate. Developers and agents with a btrfs home run
  them by default; they only touch `target/`.
- **CI:** no unprivileged route exists: btrfs isn't mountable in a user namespace (it lacks
  `FS_USERNS_MOUNT`), and a loop mount needs root. GitHub's Ubuntu runners do have
  passwordless `sudo` (<https://docs.github.com/en/actions/reference/runners/github-hosted-runners>),
  so a `snapshots-btrfs` job on `ubuntu-latest` makes a 1 GiB image (`mkfs.btrfs` from the
  flake), loop-mounts it with `sudo`, `chown`s it to the runner user, and runs the btrfs tests
  **unprivileged** against it (`GOGO_TEST_BTRFS_DIR`). The first step checks that the runner
  kernel lists btrfs in `/proc/filesystems` (after a `modprobe`) and fails with a clear
  message if not. `scripts/ci-btrfs-tests.sh` does the mount and the run twice:
  - `plain`: no `user_subvol_rm_allowed`, so the `SNAP_DESTROY_V2` ioctl gets `EPERM` and the
    `rmdir` fallback is what deletes snapshots (`btrfs_delete_through_rmdir` and the store
    test; `btrfs_delete_through_the_ioctl` takes its `EPERM` branch and checks the fallback);
  - `rm-allowed`: `-o user_subvol_rm_allowed`, so the ioctl path runs, and
    `GOGO_TEST_BTRFS_IOCTL_ALLOWED=1` makes an `EPERM` there a failure.

  Both runs set `GOGO_TEST_BTRFS_REQUIRED=1`: a test that would print `SKIP … is not on btrfs`
  panics instead, so the job can't pass by skipping everything. `btrfs-progs` (only
  `mkfs.btrfs`, for the image) is in the Linux dev shell, pinned with the rest of nixpkgs.
  The runner kernel (Ubuntu's Azure 7.0) has btrfs and loop mounts. One thing the job showed:
  with `user_subvol_rm_allowed` the ioctl refuses a read-only subvolume with `EROFS` (it
  wants write access), so `destroy` clears the read-only flag before the ioctl.
  Design 08 §6 lists the job's env hooks.
- **ZFS in CI** the same way if the runner can load the module (`zfsutils-linux` on Ubuntu,
  a file-backed pool, `zfs allow` to the runner user); that is also where §2.2's unverified
  points get settled. If it can't, ZFS is a host pass.

### 10.4 linux-e2e

The harness snapshots its scratch data root with its own subvolumes (`docs/linux-e2e.md`,
"Copies"), and a game dir made as a nested subvolume would show up empty in them. So the
harness's scratch config sets `[snapshots] auto = false`, which builds plain dirs as today.
One new step, `snapshot_rollback`, builds a cached game with snapshots on in its own scratch
dir, adds a DLC (or rebuilds), writes a fake save, rolls back, and checks that the game
launches to its ready state, the DLC set matches, and the save is in the aside dir. It runs
headless, so agents can run it on games already in the cache.

### 10.5 Host pass (the PR lists these; agents don't run them on real games)

- A real game with a big prefix on the user's btrfs: build with snapshots, `dlc add`, roll
  back, play, roll back the rollback; space numbers look right.
- A library on ext4: the build plan's and `snapshot`'s messages; `move` from `games/` (btrfs)
  to it and back: snapshots dropped with the question, `initial` taken after moving back.
- An interrupted rollback (kill during it) on a real game: `doctor`, then the rerun.
- snapper or Timeshift on `/home`: confirm the game dir drops out of their snapshots, as the
  plan line says.
- ZFS on a real pool with delegation (if the CI job can't).

## 11. Decisions

Decided on 2026-11-12: every answer below was accepted as written.

| # | Question | Decision |
|---|---|---|
| D1 | Make the game dir a subvolume by default on btrfs, given it then drops out of snapper/Timeshift snapshots and `btrfs send` backups of the subvolume around it? | **Yes**, it's what "on by default" needs; the plan line and user docs say it, and `--no-snapshots` keeps a plain dir. |
| D2 | Copy newer saves aside by default on rollback (vs only with a flag)? | **Yes, by default**; `--drop-saves` skips. Saves are small, and on ZFS it is the only copy left. |
| D3 | Always take a pre-rollback snapshot on btrfs, even with autosnapshot off? | **Yes**: it makes a rollback undoable at no cost, and it is pruned like any `auto` snapshot. |
| D4 | Restore the L4 override on rollback by default? | **Yes**, with `--keep-override`: a rebuild or `--retarget` writes keys there, so restoring it is what makes the rollback consistent. |
| D5 | `move` across filesystems: delete the snapshots (with a question) or leave them on the old drive? | **Delete**, listed with their space and confirmed; leftovers can't be rolled back to. |
| D6 | Retention: keep the last 5 automatic snapshots per game, no age or space rule in v1? | **Yes**; `keep_auto` is configurable, and space is shown. |
| D7 | ZFS: rollback to an older snapshot destroys the newer ones (`-r`), vs a slow non-atomic copy-back from `.zfs/snapshot`? | **Accept `-r`**, with the newer snapshots listed and confirmed. |
| D8 | Games built before this: convert them on their own (at the next `dlc add` or rebuild), or only when the user turns snapshots on? | **Only on request** (`auto on` / `create`), with a `doctor` hint. |
| D9 | A reflink-copy fallback on XFS? | **No** for now; the trait leaves room. |
| D10 | Allow `create` on a running game (`--allow-running`)? | **No**: a snapshot taken mid-save is the checkpoint a rollback must not hand back. |
| D11 | After a rollback, cloud saves (#275) treat every difference as a conflict to ask about? | **Yes** (§6.3); #275's design takes it as a requirement. |
| D12 | An autosnapshot before a setup that changes the game's GE-Proton? | **Yes**: a Proton upgrade changes the prefix and can't be undone otherwise. |
| D13 | Own btrfs ioctl module vs the `btrfs-uapi` crate? | **Own module** (five ioctls, already proven by the e2e helper); revisit if it grows. |
| D14 | bcachefs: implement now or on request? | **On request**: out of mainline since 6.18. |

## 12. Implementation issues (proposed; filed after this design is approved)

All under epic #155 (Linux runtime), `type:feature`, `area:wine-runtime` (+ `area:cli` where
there is CLI).

| # | Title | Scope | Depends on |
|---|---|---|---|
| I1 | snapshots: core, metadata and the btrfs backend | `snapshot/` module: trait, fake, btrfs ioctls, detection, metadata (§4), create/delete with recovery, FIEMAP space estimate; local real-btrfs tests that skip elsewhere | — |
| I2 | snapshots: CI job on a loop-mounted btrfs | `snapshots-btrfs` job (sudo loop mount, unprivileged tests, both mount options), `btrfs-progs` in the dev shell; settles whether the runner kernel has btrfs | I1 |
| I3 | linux-app: snapshots at build time and `snapshot list/create/delete` | subvolume game dirs (both lanes), `initial` snapshot, `--no-snapshots`, `[snapshots]` config, plan line, the CLI's `list`/`create`/`delete`/`auto`, `apps show`, `doctor`'s `snapshots` check, `apps remove`/`prune`, the store GC's references, `apps list` and `Library::resolve` skipping `.gogo-snapshots` | I1 |
| I4 | linux-app: `snapshot rollback` | btrfs rollback (§5.3.1) with journal and recovery, newer-saves finder and copy-aside (§6), L4 restore, `launcher::write`, launcher refusal on the markers (`LAUNCHER_VERSION` + 1) | I3 |
| I5 | linux-app: autosnapshot triggers and retention | `dlc add`, rebuild, runtime-changing setup, pre-rollback; `keep_auto` pruning; `--no-snapshots` on each; the failed-snapshot stop | I3 (I4 for the pre-rollback one) |
| I6 | linux-app: convert an existing game dir to a subvolume | §2.1's conversion behind `create`/`auto on`, with its journal and recovery | I3 |
| I7 | linux-app move: carry or drop snapshots | same-fs rename of `.gogo-snapshots/<gogid>`, cross-fs drop with `--yes`, subvolume at the target, `initial` after the move | I3 |
| I8 | linux-e2e: snapshots off in the harness, and a `snapshot_rollback` step | §10.4 | I4 |
| I9 | snapshots: ZFS backend | verify §2.2's open points on a real pool (CI if possible), detection, delegation check, the build accepting a prepared dataset, `zfs` lookup, rollback with `-r` | I4 |

Not new issues, but requirements added to existing ones (as comments when this is approved):
#360 (the update path takes `before update <old> → <new>` before its journal), #353
(`before removing <title>`), #275 (§6.3's four points). bcachefs gets no issue until someone
asks for it (D14).
