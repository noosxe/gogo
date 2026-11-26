# Bundled fonts

Both are SIL Open Font License 1.1 (the licence text sits next to each file) and were
copied unmodified from the pinned nixpkgs of this repository's `flake.lock`:

| File | nixpkgs attribute | Version | Store file |
|---|---|---|---|
| `NotoSans.ttf` | `noto-fonts` | 2026.10.01 (font version 2.015) | `share/fonts/noto/NotoSans.ttf` |
| `JetBrainsMono.ttf` | `jetbrains-mono` | 2.304 | `share/fonts/truetype/JetBrainsMono[wght].ttf` |

Noto Sans is variable on `wght` and `wdth`; JetBrains Mono is variable on `wght` only.
To update, bump nixpkgs, copy the same files and change the versions here.
