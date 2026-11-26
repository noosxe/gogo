//! Built-in skins as data, embedded with `include_bytes!` and loaded through the same public
//! loader as any other skin (design 24 §2, design 26 S17, design 27 §10).
//!
//! A [`BuiltinSkin`] is a name and a static list of `(relative path, bytes)`: the shape
//! `SkinPackage::from_files` takes. This crate depends on no other crate of the workspace.
#![deny(unsafe_code)]

/// One built-in skin: the files of its folder, with paths relative to it and `/` separators.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinSkin {
    /// The skin's name, also the name in its `skin.kdl`.
    pub name: &'static str,
    /// Every file of the skin, `skin.kdl` included.
    pub files: &'static [(&'static str, &'static [u8])],
}

impl BuiltinSkin {
    /// The files as owned data, ready for `SkinPackage::from_files(name, files)`.
    pub fn to_files(&self) -> Vec<(String, Vec<u8>)> {
        self.files
            .iter()
            .map(|(p, d)| ((*p).to_owned(), d.to_vec()))
            .collect()
    }
}

macro_rules! file {
    ($rel:literal) => {
        (
            $rel,
            include_bytes!(concat!("../skins/default/", $rel)) as &[u8],
        )
    };
}

/// The Default skin (design 26 §8): the fallback for every route.
pub const DEFAULT: BuiltinSkin = BuiltinSkin {
    name: "default",
    files: &[
        file!("skin.kdl"),
        file!("tokens.kdl"),
        file!("styles.kdl"),
        file!("components/stock.kdl"),
        file!("components/game.kdl"),
        file!("components/library.kdl"),
        file!("components/queue.kdl"),
        file!("routes/shell.kdl"),
        file!("routes/game.kdl"),
        file!("routes/sign-in.kdl"),
        file!("routes/library.kdl"),
        file!("routes/queue.kdl"),
        file!("routes/updates.kdl"),
    ],
};

/// The Default skin.
pub fn default() -> BuiltinSkin {
    DEFAULT
}

/// Every built-in skin.
pub fn builtin() -> &'static [BuiltinSkin] {
    &[DEFAULT]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
        for e in std::fs::read_dir(dir).expect("read dir").flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).expect("under root");
                out.insert(rel.to_str().expect("utf-8").replace('\\', "/"));
            }
        }
    }

    #[test]
    fn every_file_under_a_skin_folder_is_listed_and_nothing_else() {
        let skins = Path::new(env!("CARGO_MANIFEST_DIR")).join("skins");
        let mut folders = BTreeSet::new();
        for e in std::fs::read_dir(&skins).expect("skins").flatten() {
            folders.insert(e.file_name().to_str().expect("utf-8").to_owned());
        }
        let listed: BTreeSet<String> = builtin().iter().map(|s| s.name.to_owned()).collect();
        assert_eq!(folders, listed, "a skin folder and the list disagree");
        for skin in builtin() {
            let root = skins.join(skin.name);
            let mut on_disk = BTreeSet::new();
            walk(&root, &root, &mut on_disk);
            let in_list: BTreeSet<String> =
                skin.files.iter().map(|(p, _)| (*p).to_owned()).collect();
            assert_eq!(
                skin.files.len(),
                in_list.len(),
                "{}: a path twice",
                skin.name
            );
            assert_eq!(on_disk, in_list, "{}", skin.name);
            for (p, data) in skin.files {
                assert_eq!(&std::fs::read(root.join(p)).expect("read"), data, "{p}");
            }
        }
    }
}
