//! The static limits a skin is checked against at load (design 27 §8, S12). A skin that
//! exceeds one fails to load, and the default skin is used.

/// One KDL file.
pub const MAX_KDL_FILE_BYTES: usize = 256 * 1024;
/// Nodes in the whole skin, over every KDL file.
pub const MAX_NODES: usize = 20_000;
/// Levels of nested nodes. A file's top-level nodes are level 1.
pub const MAX_DEPTH: usize = 48;
/// One PNG file.
pub const MAX_PNG_BYTES: usize = 4 * 1024 * 1024;
/// One SVG file.
pub const MAX_SVG_BYTES: usize = 512 * 1024;
/// One OTF or TTF file.
pub const MAX_FONT_BYTES: usize = 8 * 1024 * 1024;
/// The whole skin folder, every file counted.
pub const MAX_SKIN_BYTES: usize = 32 * 1024 * 1024;
/// Files in the package.
pub const MAX_FILES: usize = 1024;
/// A relative path inside the package, in bytes.
pub const MAX_PATH_BYTES: usize = 200;

/// The longest the loader trusts the naive brace nesting of a KDL file before handing it to
/// the parser: the real depth limit is [`MAX_DEPTH`], this only keeps a hostile file from
/// recursing the parser's stack.
pub(crate) const PARSER_GUARD_DEPTH: usize = 4 * MAX_DEPTH;
