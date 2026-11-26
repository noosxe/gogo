//! One failing skin per rule of the loader, built in memory with `from_files`.
//!
//! Each case edits the Default skin (`crates/gogo-skins/skins/default`), loads it,
//! and checks the diagnostic's stable code, whether it fails the skin or one route, and
//! that its span points at the right text. The section names are those of design 27 and
//! the invariants those of design 26 §7.

use std::path::Path;

use gogo_skin::{Code, Level, SkinPackage};

type Files = Vec<(String, Vec<u8>)>;

fn base() -> Files {
    fn walk(root: &Path, dir: &Path, out: &mut Files) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/");
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gogo-skins/skins/default");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out
}

fn rep(f: &mut Files, path: &str, from: &str, to: &str) {
    let (_, data) = f
        .iter_mut()
        .find(|(p, _)| p == path)
        .unwrap_or_else(|| panic!("no file {path}"));
    let text = String::from_utf8(std::mem::take(data)).unwrap();
    assert!(text.contains(from), "`{from}` is not in {path}");
    *data = text.replacen(from, to, 1).into_bytes();
}

/// Every occurrence of `from` in `path`, so a token lands in each mode.
fn rep_all(f: &mut Files, path: &str, from: &str, to: &str) {
    let (_, data) = f
        .iter_mut()
        .find(|(p, _)| p == path)
        .unwrap_or_else(|| panic!("no file {path}"));
    let text = String::from_utf8(std::mem::take(data)).unwrap();
    assert!(text.contains(from), "`{from}` is not in {path}");
    *data = text.replace(from, to).into_bytes();
}

fn append(f: &mut Files, path: &str, extra: &str) {
    let (_, data) = f.iter_mut().find(|(p, _)| p == path).unwrap();
    data.extend(extra.as_bytes());
}

fn add(f: &mut Files, path: &str, data: impl Into<Vec<u8>>) {
    f.push((path.to_owned(), data.into()));
}

fn rm(f: &mut Files, path: &str) {
    let n = f.len();
    f.retain(|(p, _)| p != path);
    assert!(f.len() < n, "no file {path}");
}

fn png() -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend([0, 0, 0, 13]);
    v.extend(b"IHDR");
    v.extend([0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    v.extend([0; 4]);
    v.extend([0, 0, 0, 0]);
    v.extend(b"IEND");
    v.extend([0; 4]);
    v
}

/// A PNG whose header says `w` by `h`.
fn png_sized(w: u32, h: u32) -> Vec<u8> {
    let mut v = png();
    v[16..20].copy_from_slice(&w.to_be_bytes());
    v[20..24].copy_from_slice(&h.to_be_bytes());
    v
}

fn font() -> Vec<u8> {
    let mut v = b"OTTO\0\x01\0\0\0\0\0\0".to_vec();
    v.extend([0; 16]);
    v
}

const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 8 8\"><rect width=\"8\" height=\"8\"/></svg>";

fn load(f: Files) -> Result<SkinPackage, gogo_skin::Report> {
    SkinPackage::from_files("default", f)
}

/// Where a case's finding must land.
#[derive(Clone, Copy, Debug, PartialEq)]
enum At {
    Skin,
    Route(&'static str),
    Warn,
}

struct Case {
    /// Design 27 section (and invariant) the rule comes from.
    section: &'static str,
    name: &'static str,
    edit: fn(&mut Files),
    code: Code,
    at: At,
    /// The text the span must cover; `None` for a finding about a path.
    text: Option<&'static str>,
}

const fn case(
    section: &'static str,
    name: &'static str,
    edit: fn(&mut Files),
    code: Code,
    at: At,
    text: Option<&'static str>,
) -> Case {
    Case {
        section,
        name,
        edit,
        code,
        at,
        text,
    }
}

/// A style appended to styles.kdl.
fn style(f: &mut Files, body: &str) {
    append(f, "styles.kdl", &format!("\nstyle \"x\" {{ {body} }}\n"));
}

/// A node added to the game route, ahead of its tabs.
fn game_insert(f: &mut Files, node: &str) {
    rep(f, GAME, "    tabs ", &format!("    {node}\n    tabs "));
}

const GAME: &str = "routes/game.kdl";
const SHELL: &str = "routes/shell.kdl";

fn cases() -> Vec<Case> {
    use At::*;
    use Code::*;
    let mut v = vec![
        // §1 and S10: the layout and the paths.
        case(
            "§1 S10",
            "dotdot path",
            |f| add(f, "routes/../x.kdl", ""),
            PathInvalid,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "absolute path",
            |f| add(f, "/etc/x.kdl", ""),
            PathInvalid,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "backslash path",
            |f| add(f, "assets\\a.png", png()),
            PathInvalid,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "other file type",
            |f| add(f, "assets/run.js", "x"),
            FileType,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "file outside the layout",
            |f| add(f, "extra.kdl", ""),
            FileType,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "folder in components",
            |f| add(f, "components/sub/x.kdl", ""),
            FileType,
            Skin,
            None,
        ),
        case(
            "§1",
            "kdl in assets",
            |f| add(f, "assets/x.kdl", ""),
            FileType,
            Skin,
            None,
        ),
        case(
            "§1",
            "same path twice",
            |f| {
                add(f, "assets/a.png", png());
                add(f, "assets/a.png", png());
            },
            DuplicatePath,
            Skin,
            None,
        ),
        case(
            "§1",
            "paths differing in case",
            |f| {
                add(f, "assets/a.png", png());
                add(f, "assets/A.png", png());
            },
            DuplicatePath,
            Skin,
            None,
        ),
        // §1, S10, S11: assets.
        case(
            "§1 S10",
            "png without the signature",
            |f| add(f, "assets/a.png", "GIF89a"),
            AssetPng,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "truncated png",
            |f| add(f, "assets/a.png", &png()[..20]),
            AssetPng,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "svg with a script",
            |f| {
                add(
                    f,
                    "assets/a.svg",
                    "<svg xmlns=\"http://www.w3.org/2000/svg\"><script>x()</script></svg>",
                )
            },
            AssetSvg,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "svg with an event attribute",
            |f| {
                add(
                    f,
                    "assets/a.svg",
                    "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect onclick=\"x()\"/></svg>",
                )
            },
            AssetSvg,
            Skin,
            None,
        ),
        case(
            "§1 S11",
            "svg with an external href",
            |f| {
                add(
                    f,
                    "assets/a.svg",
                    "<svg xmlns=\"http://www.w3.org/2000/svg\"><image href=\"https://e.example/a.png\"/></svg>",
                )
            },
            AssetSvg,
            Skin,
            None,
        ),
        case(
            "§1 S11",
            "svg with an external url()",
            |f| {
                add(
                    f,
                    "assets/a.svg",
                    "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect fill=\"url(http://e.example/x)\"/></svg>",
                )
            },
            AssetSvg,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "svg that is animated",
            |f| {
                add(
                    f,
                    "assets/a.svg",
                    "<svg xmlns=\"http://www.w3.org/2000/svg\"><animate/></svg>",
                )
            },
            AssetSvg,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "font that is not one",
            |f| add(f, "assets/a.ttf", "not a font"),
            AssetFont,
            Skin,
            None,
        ),
        // §8 and S12: limits.
        case(
            "§8 S12",
            "kdl file over 256 KiB",
            |f| append(f, GAME, &format!("\n// {}\n", "x".repeat(300_000))),
            LimitFile,
            Skin,
            None,
        ),
        case(
            "§8 S12",
            "png over 4 MiB",
            |f| add(f, "assets/a.png", vec![0u8; 4 * 1024 * 1024 + 1]),
            LimitFile,
            Skin,
            None,
        ),
        case(
            "§8 S12",
            "svg over 512 KiB",
            |f| add(f, "assets/a.svg", vec![b' '; 512 * 1024 + 1]),
            LimitFile,
            Skin,
            None,
        ),
        case(
            "§8 S12",
            "font over 8 MiB",
            |f| add(f, "assets/a.otf", vec![0u8; 8 * 1024 * 1024 + 1]),
            LimitFile,
            Skin,
            None,
        ),
        case(
            "§8 S12",
            "folder over 32 MiB",
            |f| {
                for i in 0..5 {
                    let mut data = font();
                    data.resize(8 * 1024 * 1024, 0);
                    add(f, &format!("assets/f{i}.ttf"), data);
                }
            },
            LimitFolder,
            Skin,
            None,
        ),
        case(
            "§8 S12",
            "more than 1024 files",
            |f| {
                for i in 0..1030 {
                    add(f, &format!("assets/f{i}.svg"), SVG);
                }
            },
            LimitFiles,
            Skin,
            None,
        ),
        case(
            "§8 S12",
            "more than 20000 nodes",
            |f| {
                let body = "spacer\n".repeat(20_001);
                rm(f, SHELL);
                add(f, SHELL, format!("route \"shell\" {{\n{body}outlet\n}}\n"));
            },
            LimitNodes,
            Skin,
            None,
        ),
        case(
            "§8 S12",
            "nested deeper than 48",
            |f| {
                let n = 49;
                rm(f, GAME);
                add(
                    f,
                    GAME,
                    format!(
                        "route \"game\" {{\n{}{}}}\n",
                        "column {\n".repeat(n),
                        "}\n".repeat(n)
                    ),
                );
            },
            LimitDepth,
            Skin,
            None,
        ),
        case(
            "§8 S12",
            "nesting that would overflow the parser",
            |f| {
                let n = 20_000;
                rm(f, GAME);
                add(
                    f,
                    GAME,
                    format!(
                        "route \"game\" {{\n{}{}}}\n",
                        "a {\n".repeat(n),
                        "}\n".repeat(n)
                    ),
                );
            },
            LimitDepth,
            Skin,
            None,
        ),
        case(
            "§8 S12",
            "expression over 512 characters",
            |f| {
                rep(
                    f,
                    GAME,
                    "{g.title}\" style=\"crumb\"",
                    &format!("{{{}}}\" style=\"crumb\"", "a".repeat(600)),
                );
            },
            Expression,
            Route("game"),
            None,
        ),
        case(
            "§8 S12",
            "expression nested over 32 levels",
            |f| {
                rep(
                    f,
                    GAME,
                    "{g.title}\" style=\"crumb\"",
                    &format!(
                        "{{{}1{}}}\" style=\"crumb\"",
                        "(".repeat(40),
                        ")".repeat(40)
                    ),
                );
            },
            Expression,
            Route("game"),
            None,
        ),
        case(
            "§8 S12",
            "components that expand past the node limit",
            |f| {
                // Eleven levels of three uses each: 3^11 > 20000 nodes once expanded.
                let mut text = String::from("component \"c0\" { spacer }\n");
                for i in 1..=11 {
                    text.push_str(&format!(
                        "component \"c{i}\" {{ use \"c{}\"; use \"c{}\"; use \"c{}\" }}\n",
                        i - 1,
                        i - 1,
                        i - 1
                    ));
                }
                add(f, "components/big.kdl", text);
            },
            LimitNodes,
            Skin,
            None,
        ),
        // §2: the manifest.
        case(
            "§2",
            "no title",
            |f| rep(f, "skin.kdl", "    title \"Default\"\n", ""),
            ManifestStructure,
            Skin,
            None,
        ),
        case(
            "§2",
            "unknown manifest field",
            |f| rep(f, "skin.kdl", "    title", "    colour \"x\"\n    title"),
            ManifestStructure,
            Skin,
            Some("colour"),
        ),
        case(
            "§2",
            "version that is not semver",
            |f| rep(f, "skin.kdl", "\"0.1.0\"", "\"1.0\""),
            ManifestValue,
            Skin,
            Some("\"1.0\""),
        ),
        case(
            "§2",
            "name is not the folder",
            |f| rep(f, "skin.kdl", "skin \"default\"", "skin \"other\""),
            SkinName,
            Skin,
            Some("\"other\""),
        ),
        case(
            "§2",
            "name with capitals",
            |f| rep(f, "skin.kdl", "skin \"default\"", "skin \"Default\""),
            SkinName,
            Skin,
            Some("\"Default\""),
        ),
        case(
            "§2 S16",
            "contract that is not MAJOR.MINOR",
            |f| rep(f, "skin.kdl", "\"1.0\"", "\"one\""),
            ContractVersion,
            Skin,
            Some("\"one\""),
        ),
        case(
            "§2 S16",
            "contract of another major",
            |f| rep(f, "skin.kdl", "contract \"1.0\"", "contract \"2.0\""),
            ContractVersion,
            Skin,
            Some("\"2.0\""),
        ),
        case(
            "§2 S16",
            "contract newer than the app",
            |f| rep(f, "skin.kdl", "contract \"1.0\"", "contract \"1.7\""),
            ContractVersion,
            Skin,
            Some("\"1.7\""),
        ),
        case(
            "§2",
            "unknown mode",
            |f| rep(f, "skin.kdl", "modes \"dark\"", "modes \"dark\" \"sepia\""),
            ManifestValue,
            Skin,
            Some("\"sepia\""),
        ),
        case(
            "§2 S16",
            "unknown route in the manifest",
            |f| rep(f, "skin.kdl", "\"game\"", "\"gam\""),
            UnknownRoute,
            Skin,
            Some("\"gam\""),
        ),
        case(
            "§2",
            "covered route without a file",
            |f| rm(f, "routes/queue.kdl"),
            RouteFile,
            Skin,
            None,
        ),
        case(
            "§2",
            "route file that is not a contract route",
            |f| add(f, "routes/lobby.kdl", "route \"lobby\" { }"),
            UnknownRoute,
            Skin,
            None,
        ),
        case(
            "§2",
            "skin extends itself",
            |f| {
                rep(
                    f,
                    "skin.kdl",
                    "    title",
                    "    extends \"default\"\n    title",
                )
            },
            ManifestValue,
            Skin,
            Some("\"default\""),
        ),
        case(
            "§2",
            "ui var of an unknown type",
            |f| rep(f, "skin.kdl", "type=\"enum(grid list)\"", "type=\"colour\""),
            UiVar,
            Skin,
            Some("\"colour\""),
        ),
        case(
            "§2",
            "ui var default outside its enum",
            |f| rep(f, "skin.kdl", "default=\"grid\"", "default=\"table\""),
            UiVar,
            Skin,
            Some("\"table\""),
        ),
        case(
            "§2",
            "section var with no such section",
            |f| rep(f, "skin.kdl", "default=\"overview\"", "default=\"nowhere\""),
            UiVar,
            Skin,
            Some("\"nowhere\""),
        ),
        case(
            "§2",
            "ui var declared twice",
            |f| {
                rep(
                    f,
                    "skin.kdl",
                    "        var \"view\"",
                    "        var \"tab\" type=\"bool\" default=#true\n        var \"view\"",
                )
            },
            UiVar,
            Skin,
            None,
        ),
        // §3: tokens.
        case(
            "§3",
            "colour that is not a colour",
            |f| {
                rep(
                    f,
                    "tokens.kdl",
                    "\"#7aa2ff\"\n    on-accent",
                    "\"#7aa2f\"\n    on-accent",
                )
            },
            TokenValue,
            Skin,
            Some("\"#7aa2f\""),
        ),
        case(
            "§3",
            "reference to nothing",
            |f| rep(f, "tokens.kdl", "\"@success\"", "\"@sucess\""),
            TokenRef,
            Skin,
            Some("\"@sucess\""),
        ),
        case(
            "§3",
            "references in a loop",
            |f| {
                rep(
                    f,
                    "tokens.kdl",
                    "    scrim",
                    "    ping \"@pong\"\n    pong \"@ping\"\n    scrim",
                )
            },
            TokenRef,
            Skin,
            None,
        ),
        case(
            "§3",
            "a required token missing",
            |f| rep(f, "tokens.kdl", "    space.4 16\n", ""),
            TokenMissing,
            Skin,
            None,
        ),
        case(
            "§3",
            "required token of the wrong type",
            |f| rep(f, "tokens.kdl", "radius.md 8", "radius.md \"#ffffff\""),
            TokenValue,
            Skin,
            None,
        ),
        case(
            "§3",
            "modes that define different tokens",
            |f| {
                rep(f, "skin.kdl", "modes \"dark\"", "modes \"dark\" \"light\"");
                let dark =
                    String::from_utf8(f.iter().find(|(p, _)| p == "tokens.kdl").unwrap().1.clone())
                        .unwrap();
                append(
                    f,
                    "tokens.kdl",
                    &dark
                        .replace("tokens \"dark\"", "tokens \"light\"")
                        .replace("    scrim", "    extra \"#ffffff\"\n    scrim"),
                );
            },
            TokenMode,
            Skin,
            None,
        ),
        case(
            "§3",
            "a listed mode without tokens",
            |f| rep(f, "skin.kdl", "modes \"dark\"", "modes \"dark\" \"light\""),
            TokenStructure,
            Skin,
            None,
        ),
        case(
            "§3",
            "tokens for a mode the manifest lacks",
            |f| append(f, "tokens.kdl", "tokens \"light\" { }\n"),
            TokenMode,
            Skin,
            None,
        ),
        case(
            "§3 D3",
            "font family that is not bundled",
            |f| {
                rep(
                    f,
                    "tokens.kdl",
                    "family=\"Noto Sans\" size=14",
                    "family=\"Comic Sans\" size=14",
                )
            },
            FontFamily,
            Skin,
            Some("\"Comic Sans\""),
        ),
        case(
            "§3 D3",
            "the old nerd font family",
            |f| {
                rep(
                    f,
                    "tokens.kdl",
                    "\"JetBrains Mono\"",
                    "\"JetBrainsMono Nerd Font\"",
                )
            },
            FontFamily,
            Skin,
            Some("\"JetBrainsMono Nerd Font\""),
        ),
        case(
            "§3 §5.2",
            "stretch on a static family",
            |f| {
                rep(
                    f,
                    "tokens.kdl",
                    "family=\"JetBrains Mono\" size=13 weight=400",
                    "family=\"JetBrains Mono\" size=13 weight=400 stretch=90",
                )
            },
            FontStretch,
            Skin,
            Some("90"),
        ),
        case(
            "§3",
            "a type role's field defined alone",
            |f| {
                rep(
                    f,
                    "tokens.kdl",
                    "    scrim",
                    "    font.body.size 14\n    scrim",
                )
            },
            TokenName,
            Skin,
            Some("font.body.size"),
        ),
        case(
            "§3",
            "a font without a size",
            |f| {
                rep(
                    f,
                    "tokens.kdl",
                    "family=\"Noto Sans\" size=18 weight=700",
                    "family=\"Noto Sans\" weight=700",
                )
            },
            TokenValue,
            Skin,
            None,
        ),
        // §4 and §6: styles.
        case(
            "§4",
            "unknown style property",
            |f| {
                rep(
                    f,
                    "styles.kdl",
                    "style \"page\" { fill @bg0 }",
                    "style \"page\" { fil @bg0 }",
                )
            },
            UnknownProperty,
            Skin,
            Some("fil"),
        ),
        case(
            "§4",
            "value of the wrong type",
            |f| rep(f, "styles.kdl", "grow 1;", "grow \"wide\";"),
            PropertyValue,
            Skin,
            Some("\"wide\""),
        ),
        case(
            "§4",
            "too many values",
            |f| rep(f, "styles.kdl", "padding 0 5 }", "padding 0 5 1 2 3 }"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "unknown state",
            |f| style(f, ":active { fill @bg0 }"),
            UnknownState,
            Skin,
            Some(":active"),
        ),
        case(
            "§4",
            "unknown token in a style",
            |f| {
                rep(
                    f,
                    "styles.kdl",
                    "style \"page\" { fill @bg0 }",
                    "style \"page\" { fill @bg9 }",
                )
            },
            TokenRef,
            Skin,
            Some("@bg9"),
        ),
        case(
            "§4",
            "token of the wrong type in a style",
            |f| {
                rep(
                    f,
                    "styles.kdl",
                    "style \"page\" { fill @bg0 }",
                    "style \"page\" { fill @space.4 }",
                )
            },
            TokenRef,
            Skin,
            Some("@space.4"),
        ),
        case(
            "§4",
            "style defined twice",
            |f| append(f, "styles.kdl", "style \"page\" { fill @bg1 }\n"),
            StyleName,
            Skin,
            Some("\"page\""),
        ),
        case(
            "§4",
            "style named with capitals",
            |f| append(f, "styles.kdl", "style \"Page2\" { }\n"),
            StyleName,
            Skin,
            Some("\"Page2\""),
        ),
        case(
            "§4",
            "style that is used and not defined",
            |f| {
                rep(
                    f,
                    GAME,
                    "style=\"crumb\" id=\"crumb\"",
                    "style=\"crumbs\" id=\"crumb\"",
                )
            },
            UnknownStyle,
            Route("game"),
            Some("\"crumbs\""),
        ),
        case(
            "§4 §5.2",
            "stretch on a static family in a style",
            |f| style(f, "family \"JetBrains Mono\"; stretch 90"),
            FontStretch,
            Skin,
            Some("90"),
        ),
        case(
            "§4",
            "family that is not bundled in a style",
            |f| style(f, "family \"Liberation Sans\""),
            FontFamily,
            Skin,
            Some("\"Liberation Sans\""),
        ),
        case(
            "§4",
            "a state declaration with a nested state",
            |f| style(f, ":on { :hover { fill @bg0 } }"),
            StyleStructure,
            Skin,
            None,
        ),
        case(
            "§4",
            "an expression in a number slot of a style",
            |f| style(f, "gap \"{1}\""),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "a property set twice",
            |f| style(f, "fill @bg0; fill @bg1"),
            DuplicateProperty,
            Warn,
            Some("fill"),
        ),
        case(
            "§4 §3",
            "a short hex colour",
            |f| style(f, "fill \"#0008\""),
            PropertyValue,
            Skin,
            Some("\"#0008\""),
        ),
        // §6: effects.
        case(
            "§6",
            "shadow without a colour",
            |f| style(f, "shadow 0 6 18"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§6",
            "shadow with a negative blur",
            |f| style(f, "shadow 0 6 -1 \"#000000\""),
            PropertyValue,
            Skin,
            Some("-1"),
        ),
        case(
            "§6",
            "glow with an odd number of values",
            |f| style(f, "glow 6 \"#ffffff\" 18"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§6",
            "glow with too many layers",
            |f| style(f, "glow 1 @bg0 2 @bg0 3 @bg0 4 @bg0 5 @bg0"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§6",
            "noise amount over 1",
            |f| style(f, "noise 2"),
            PropertyValue,
            Skin,
            Some("2"),
        ),
        case(
            "§6",
            "scanlines without an alpha",
            |f| style(f, "scanlines 3"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§6",
            "reflection with a missing alpha",
            |f| style(f, "reflection 120"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§6",
            "blur-behind negative",
            |f| style(f, "blur-behind -4"),
            PropertyValue,
            Skin,
            Some("-4"),
        ),
        case(
            "§6",
            "gradient with one colour",
            |f| style(f, "gradient linear 180 @bg1"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§6",
            "gradient of an unknown kind",
            |f| style(f, "gradient conic @bg1 @bg0"),
            PropertyValue,
            Skin,
            Some("conic"),
        ),
        case(
            "§6",
            "effects with a value other than none",
            |f| style(f, "effects 1"),
            PropertyValue,
            Skin,
            Some("1"),
        ),
        // §5: elements.
        case(
            "§5",
            "unknown element",
            |f| {
                rep(
                    f,
                    SHELL,
                    "        outlet",
                    "        banner \"x\"\n        outlet",
                )
            },
            UnknownElement,
            Route("shell"),
            Some("banner"),
        ),
        case(
            "§5.1",
            "scroll",
            |f| {
                rep(
                    f,
                    SHELL,
                    "        outlet",
                    "        scroll { }\n        outlet",
                )
            },
            PlannedElement,
            Route("shell"),
            Some("scroll"),
        ),
        case(
            "§5.1",
            "outlet outside the shell",
            |f| rep(f, GAME, "    tabs ", "    outlet\n    tabs "),
            ElementPlacement,
            Route("game"),
            Some("outlet"),
        ),
        case(
            "§5.1",
            "outlet in a component",
            |f| append(f, "components/stock.kdl", "component \"o\" { outlet }\n"),
            ElementPlacement,
            Skin,
            Some("outlet"),
        ),
        case(
            "§5.4",
            "data-table holding something else",
            |f| {
                rep(
                    f,
                    GAME,
                    "highlight=\"{row.this_machine}\" {",
                    "highlight=\"{row.this_machine}\" {\n text \"x\"",
                )
            },
            ElementChildren,
            Route("game"),
            Some("text"),
        ),
        case(
            "§5.4",
            "use of data-table",
            |f| rep(f, GAME, "    tabs ", "    use \"data-table\"\n    tabs "),
            DataTableUse,
            Route("game"),
            Some("\"data-table\""),
        ),
        case(
            "§5.4",
            "data-table without rows",
            |f| rep(f, GAME, "rows=\"{game.builds($id) | take 4}\" ", ""),
            ElementArguments,
            Route("game"),
            None,
        ),
        case(
            "§5.5",
            "part outside an app widget",
            |f| rep(f, GAME, "    tabs ", "    part \"frame\"\n    tabs "),
            ElementPlacement,
            Route("game"),
            Some("part"),
        ),
        case(
            "§5.3",
            "item outside a menu",
            |f| rep(f, GAME, "    tabs ", "    item \"x\"\n    tabs "),
            ElementPlacement,
            Route("game"),
            Some("item"),
        ),
        case(
            "§5.3",
            "args using value on a button",
            |f| {
                rep(
                    f,
                    GAME,
                    "action=\"game.verify\" args=\"{$id}\"",
                    "action=\"game.verify\" args=\"{$id} {value}\"",
                )
            },
            InputScope,
            Route("game"),
            Some("{value}"),
        ),
        case(
            "§5.3",
            "value on something that is not a select",
            |f| {
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    text \"x\" value=\"{a}\"\n    tabs ",
                )
            },
            UnknownProperty,
            Route("game"),
            Some("value"),
        ),
        case(
            "§5.3 S14",
            "button without a name",
            |f| {
                rep(
                    f,
                    SHELL,
                    "button \"Settings\" icon=\"circle\"",
                    "button icon=\"circle\"",
                )
            },
            AccessibleName,
            Route("shell"),
            Some("button"),
        ),
        case(
            "§5.3 S14",
            "button with an empty name",
            |f| {
                rep(
                    f,
                    SHELL,
                    "button \"Settings\" icon=\"circle\"",
                    "button \"\" icon=\"circle\"",
                )
            },
            AccessibleName,
            Route("shell"),
            Some("button"),
        ),
        case(
            "§5.2 S14",
            "art without a label",
            |f| rep(f, GAME, " label=\"{g.title} cover\"", ""),
            AccessibleName,
            Route("game"),
            Some("art"),
        ),
        case(
            "§5.3 S14",
            "toggle outside a field without a label",
            |f| {
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    toggle on=\"{snaps.auto}\"\n    tabs ",
                )
            },
            AccessibleName,
            Route("game"),
            Some("toggle"),
        ),
        case(
            "§5.8 S20",
            "each over a literal",
            |f| {
                rep(
                    f,
                    GAME,
                    "in=\"{snaps.items | sort -date | take 2}\"",
                    "in=\"snaps\"",
                )
            },
            EachSource,
            Route("game"),
            Some("\"snaps\""),
        ),
        case(
            "§5.8 S20",
            "each without in",
            |f| rep(f, GAME, " in=\"{snaps.items | sort -date | take 2}\"", ""),
            EachSource,
            Route("game"),
            Some("each"),
        ),
        case(
            "§5.7 S20",
            "components that use each other",
            |f| {
                append(
                    f,
                    "components/stock.kdl",
                    "component \"ping\" { use \"pong\" }\ncomponent \"pong\" { use \"ping\" }\n",
                )
            },
            ComponentCycle,
            Skin,
            None,
        ),
        case(
            "§5.7 S20",
            "a component that uses itself",
            |f| {
                append(
                    f,
                    "components/stock.kdl",
                    "component \"me\" { use \"me\" }\n",
                )
            },
            ComponentCycle,
            Skin,
            None,
        ),
        case(
            "§5.7",
            "use of an unknown component",
            |f| {
                rep(
                    f,
                    GAME,
                    "use \"summary-tile\" mark=\"check\"",
                    "use \"summary-tle\" mark=\"check\"",
                )
            },
            UnknownComponent,
            Route("game"),
            Some("\"summary-tle\""),
        ),
        case(
            "§5.7",
            "use with an unknown parameter",
            |f| rep(f, GAME, " title=\"DLC", " titel=\"DLC"),
            ComponentParam,
            Route("game"),
            Some("titel"),
        ),
        case(
            "§5.7",
            "component with a reserved parameter",
            |f| {
                append(
                    f,
                    "components/stock.kdl",
                    "component \"r\" params=\"when\" { }\n",
                )
            },
            ComponentParam,
            Skin,
            None,
        ),
        case(
            "§5.7",
            "component defined twice",
            |f| {
                append(
                    f,
                    "components/stock.kdl",
                    "component \"status-badge\" { }\n",
                )
            },
            ComponentParam,
            Skin,
            Some("\"status-badge\""),
        ),
        case(
            "§5",
            "unknown element property",
            |f| {
                rep(
                    f,
                    GAME,
                    "style=\"crumb\" id=\"crumb\"",
                    "style=\"crumb\" id=\"crumb\" colr=@accent",
                )
            },
            UnknownProperty,
            Route("game"),
            Some("colr"),
        ),
        case(
            "§5.3 S16",
            "unknown action",
            |f| rep(f, GAME, "action=\"game.verify\"", "action=\"game.verfy\""),
            UnknownAction,
            Route("game"),
            Some("\"game.verfy\""),
        ),
        case(
            "§5.2",
            "unknown icon",
            |f| rep(f, GAME, "icon=\"play\"", "icon=\"plai\""),
            PropertyValue,
            Route("game"),
            Some("\"plai\""),
        ),
        case(
            "§5.2",
            "arguments an element doesn't take",
            |f| rep(f, GAME, "    tabs ", "    row \"x\"\n    tabs "),
            ElementArguments,
            Route("game"),
            None,
        ),
        case(
            "§5",
            "a required property missing",
            |f| {
                rep(
                    f,
                    GAME,
                    " sections=\"overview builds dlc saves snapshots setup files\"",
                    "",
                )
            },
            ElementArguments,
            Route("game"),
            None,
        ),
        case(
            "§5.2 S10",
            "image of an asset that is not there",
            |f| {
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    image src=\"assets/none.png\" label=\"x\"\n    tabs ",
                )
            },
            AssetMissing,
            Route("game"),
            Some("\"assets/none.png\""),
        ),
        case(
            "§5.2 S11",
            "image from a URL",
            |f| {
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    image src=\"https://e.example/a.png\" label=\"x\"\n    tabs ",
                )
            },
            UrlScheme,
            Route("game"),
            Some("\"https://e.example/a.png\""),
        ),
        case(
            "§5.2 S10",
            "image path that escapes the skin",
            |f| {
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    image src=\"../a.png\" label=\"x\"\n    tabs ",
                )
            },
            PathInvalid,
            Route("game"),
            Some("\"../a.png\""),
        ),
        case(
            "§5.2",
            "image path from an expression",
            |f| {
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    image src=\"{g.title}\" label=\"x\"\n    tabs ",
                )
            },
            AssetDynamic,
            Route("game"),
            Some("\"{g.title}\""),
        ),
        case(
            "§5.2",
            "image of a font file",
            |f| {
                add(f, "assets/f.ttf", font());
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    image src=\"assets/f.ttf\" label=\"x\"\n    tabs ",
                );
            },
            AssetMissing,
            Route("game"),
            Some("\"assets/f.ttf\""),
        ),
        case(
            "§5.8",
            "case with a number",
            |f| rep(f, GAME, "case \"builds\"", "case 1"),
            ElementArguments,
            Route("game"),
            None,
        ),
        case(
            "§5.8",
            "a case that can never match",
            |f| rep(f, GAME, "case \"builds\"", "case \"overview\""),
            DuplicateCase,
            Warn,
            Some("\"overview\""),
        ),
        case(
            "§5.8",
            "switch holding something else",
            |f| {
                rep(
                    f,
                    GAME,
                    "        else { use \"game.files\"",
                    "        text \"x\"\n        else { use \"game.files\"",
                )
            },
            ElementChildren,
            Route("game"),
            Some("text"),
        ),
        case(
            "§5.8",
            "let binding a name twice",
            |f| {
                rep(
                    f,
                    GAME,
                    "    let g=\"{game($id)}\"",
                    "    let g=\"{game($id)}\" g=\"{game($id)}\"",
                )
            },
            PropertyValue,
            Route("game"),
            Some("g"),
        ),
        case(
            "§5.8",
            "switch on a plain string",
            |f| rep(f, GAME, "switch \"{ui.tab}\"", "switch \"tab\""),
            ElementArguments,
            Route("game"),
            None,
        ),
        case(
            "S18",
            "a status shown by colour alone",
            |f| {
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    chip \"x\" tone=\"{g.status}\"\n    tabs ",
                )
            },
            StatusByColour,
            Warn,
            Some("\"{g.status}\""),
        ),
        case(
            "§5.7 §9",
            "an error in a component fails the skin",
            |f| rep(f, "components/stock.kdl", "icon=\"dot\"", "icon=\"dott\""),
            PropertyValue,
            Skin,
            Some("\"dott\""),
        ),
        // §7 and §9 step 3: templates and expressions are parsed at load.
        case(
            "§7",
            "a brace that is never closed",
            |f| {
                rep(
                    f,
                    GAME,
                    "{g.title}\" style=\"crumb\"",
                    "{g.title\" style=\"crumb\"",
                )
            },
            Template,
            Route("game"),
            Some("{"),
        ),
        case(
            "§7",
            "a lone closing brace",
            |f| rep(f, GAME, "Library  ›  ", "Library } "),
            Template,
            Route("game"),
            Some("}"),
        ),
        case(
            "§7",
            "an expression that doesn't parse",
            |f| {
                rep(
                    f,
                    GAME,
                    "{g.title}\" style=\"crumb\"",
                    "{g.title +}\" style=\"crumb\"",
                )
            },
            Expression,
            Route("game"),
            Some("+"),
        ),
        case(
            "§7",
            "an expression in an action argument",
            |f| {
                rep(
                    f,
                    GAME,
                    "args=\"game {$id} files\"",
                    "args=\"game {$id +} files\"",
                )
            },
            Expression,
            Route("game"),
            None,
        ),
        case(
            "§7",
            "an unknown pipe stage",
            |f| rep(f, GAME, "sort -date", "sortt -date"),
            Expression,
            Route("game"),
            Some("sortt"),
        ),
        case(
            "§7",
            "an expression in a style colour",
            |f| style(f, "fill \"{@warn alpha=}\""),
            Expression,
            Skin,
            None,
        ),
        // The route file itself.
        case(
            "§1",
            "route file that names another route",
            |f| rep(f, GAME, "route \"game\"", "route \"queue\""),
            RouteFile,
            Route("game"),
            Some("\"queue\""),
        ),
        case(
            "§1",
            "route file with two nodes",
            |f| append(f, GAME, "route \"game\" { }\n"),
            RouteFile,
            Route("game"),
            None,
        ),
        case(
            "§9",
            "KDL that doesn't parse, in a route",
            |f| append(f, GAME, "\ntext \"unterminated\n"),
            KdlSyntax,
            Route("game"),
            None,
        ),
        case(
            "§9",
            "KDL that doesn't parse, in the tokens",
            |f| append(f, "tokens.kdl", "\ntokens \"dark\" {\n"),
            KdlSyntax,
            Skin,
            None,
        ),
        case(
            "§9",
            "KDL v1 syntax",
            |f| rep(f, "skin.kdl", "modes \"dark\"", "modes \"dark\" true"),
            KdlSyntax,
            Skin,
            Some("true"),
        ),
        // Review gaps: switch, paths, manifest, ranges, effects, elements, tokens.
        case(
            "§5.8",
            "a second else in a switch",
            |f| {
                game_insert(
                    f,
                    "switch \"{ui.tab}\" {\n case \"a\" { }\n else { }\n else { }\n }",
                )
            },
            ElementChildren,
            Route("game"),
            Some("else"),
        ),
        case(
            "§5.8",
            "a case after the else",
            |f| game_insert(f, "switch \"{ui.tab}\" {\n else { }\n case \"a\" { }\n }"),
            ElementChildren,
            Route("game"),
            Some("case"),
        ),
        case(
            "§5.8",
            "an else with arguments",
            |f| {
                game_insert(
                    f,
                    "switch \"{ui.tab}\" {\n case \"a\" { }\n else \"x\" { }\n }",
                )
            },
            ElementArguments,
            Route("game"),
            None,
        ),
        case(
            "§1 S10",
            "a path with a NUL byte",
            |f| add(f, "assets/a\0.png", png()),
            PathInvalid,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "a path with a control character",
            |f| add(f, "assets/a\u{7}.png", png()),
            PathInvalid,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "a path over 200 bytes",
            |f| add(f, &format!("assets/{}.png", "a".repeat(200)), png()),
            PathInvalid,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "a path with a colon",
            |f| add(f, "assets/a:b.png", png()),
            PathInvalid,
            Skin,
            None,
        ),
        case(
            "§2",
            "a mode listed twice",
            |f| rep(f, "skin.kdl", "modes \"dark\"", "modes \"dark\" \"dark\""),
            ManifestValue,
            Skin,
            Some("\"dark\""),
        ),
        case(
            "§2",
            "no modes",
            |f| rep(f, "skin.kdl", "modes \"dark\"", "modes"),
            ManifestValue,
            Skin,
            None,
        ),
        case(
            "§2",
            "no version",
            |f| rep(f, "skin.kdl", "    version \"0.1.0\"\n", ""),
            ManifestStructure,
            Skin,
            None,
        ),
        case(
            "§2",
            "no contract",
            |f| rep(f, "skin.kdl", "    contract \"1.0\"\n", ""),
            ManifestStructure,
            Skin,
            None,
        ),
        case(
            "§2",
            "a field written twice",
            |f| {
                rep(
                    f,
                    "skin.kdl",
                    "    title \"Default\"\n",
                    "    title \"Default\"\n    title \"Again\"\n",
                )
            },
            ManifestStructure,
            Skin,
            Some("title"),
        ),
        case(
            "§2",
            "a name over 64 characters",
            |f| {
                rep(
                    f,
                    "skin.kdl",
                    "skin \"default\"",
                    &format!("skin \"{}\"", "a".repeat(65)),
                )
            },
            SkinName,
            Skin,
            None,
        ),
        // §4: every range the loader enforces.
        case(
            "§4",
            "opacity over 1",
            |f| style(f, "opacity 1.5"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "weight over 1000",
            |f| style(f, "weight 1001"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "stretch over 200",
            |f| style(f, "stretch 201"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "font-size under 1",
            |f| style(f, "font-size 0"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "tracking over 5",
            |f| style(f, "tracking 6"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "line-height under 0.1",
            |f| style(f, "line-height 0"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "aspect of zero",
            |f| style(f, "aspect 0"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "a negative grow",
            |f| style(f, "grow -1"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "lines over 1000",
            |f| style(f, "lines 1001"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "col-span over 64",
            |f| style(f, "col-span 65"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "more than 64 columns",
            |f| style(f, "columns 65"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "padding with three values",
            |f| style(f, "padding 1 2 3"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "radius with two values",
            |f| style(f, "radius 1 2"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "gap with three values",
            |f| style(f, "gap 1 2 3"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4 §6",
            "a gradient over eight stops",
            |f| {
                style(
                    f,
                    "gradient radial @bg0 @bg1 @bg0 @bg1 @bg0 @bg1 @bg0 @bg1 @bg0",
                )
            },
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4 §6",
            "a linear gradient angle over 3600",
            |f| style(f, "gradient linear 3601 @bg0 @bg1"),
            PropertyValue,
            Skin,
            None,
        ),
        // §6: effects.
        case(
            "§6",
            "a shadow spread out of range",
            |f| style(f, "shadow 0 0 4 \"#000000\" 10001"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§6",
            "a noise seed over u32",
            |f| style(f, "noise 0.1 4294967296"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§6",
            "a scanlines period over 1000",
            |f| style(f, "scanlines 1001 0.2"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§6",
            "a reflection alpha over 1",
            |f| style(f, "reflection 90 1.5"),
            PropertyValue,
            Skin,
            None,
        ),
        // §5: elements.
        case(
            "§5.3",
            "tabs without bind",
            |f| game_insert(f, "tabs sections=\"overview builds\""),
            ElementArguments,
            Route("game"),
            None,
        ),
        case(
            "§5.3",
            "select without options",
            |f| game_insert(f, "field \"Pick\" { select value=\"{ui.view}\" }"),
            ElementArguments,
            Route("game"),
            None,
        ),
        case(
            "§5.4",
            "col with a template and a body",
            |f| {
                game_insert(
                    f,
                    "data-table label=\"B\" rows=\"{game.builds($id)}\" { col \"A\" \"{row.version}\" { text \"x\" } }",
                )
            },
            ElementChildren,
            Route("game"),
            None,
        ),
        case(
            "§5.4",
            "data-table with no col",
            |f| game_insert(f, "data-table label=\"B\" rows=\"{game.builds($id)}\""),
            ElementChildren,
            Route("game"),
            None,
        ),
        case(
            "§5.3",
            "link-action without link",
            |f| {
                game_insert(
                    f,
                    "card title=\"X\" link-action=\"ui.set\" link-args=\"tab builds\"",
                )
            },
            AccessibleName,
            Route("game"),
            None,
        ),
        case(
            "§5.8",
            "an id with a space",
            |f| game_insert(f, "text \"x\" id=\"a b\""),
            PropertyValue,
            Route("game"),
            Some("\"a b\""),
        ),
        case(
            "§5.8",
            "an each variable that is not a name",
            |f| game_insert(f, "each \"A b\" in=\"{game.builds($id)}\" { }"),
            ElementArguments,
            Route("game"),
            None,
        ),
        case(
            "§5.2",
            "art with a fit that does not exist",
            |f| game_insert(f, "art \"{g.art.cover}\" fit=\"zoom\" label=\"x\""),
            PropertyValue,
            Route("game"),
            Some("\"zoom\""),
        ),
        case(
            "§5.6",
            "a progress that is not an expression",
            |f| game_insert(f, "box style=\"bar-fill\" progress=\"0.5\""),
            PropertyValue,
            Route("game"),
            Some("\"0.5\""),
        ),
        // §5.3: an action is a plain name.
        case(
            "§5.3 S16",
            "a button whose action is a template",
            |f| game_insert(f, "button \"Go\" action=\"{$id}\""),
            ActionDynamic,
            Route("game"),
            Some("\"{$id}\""),
        ),
        case(
            "§5.3 S16",
            "a card whose link-action is a template",
            |f| {
                game_insert(
                    f,
                    "card title=\"X\" link=\"More\" link-action=\"{$id}\" link-args=\"tab builds\"",
                )
            },
            ActionDynamic,
            Route("game"),
            Some("\"{$id}\""),
        ),
        case(
            "§5.3 S16",
            "a component action given a template",
            |f| {
                game_insert(
                    f,
                    "use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"{$id}\"",
                )
            },
            ActionDynamic,
            Route("game"),
            Some("\"{$id}\""),
        ),
        case(
            "§5.3 S16",
            "a component action that is not a parameter",
            |f| {
                rep(
                    f,
                    "components/stock.kdl",
                    "action=\"{action}\"",
                    "action=\"{$id}\"",
                )
            },
            ActionDynamic,
            Skin,
            Some("\"{$id}\""),
        ),
        // §3: tokens.
        case(
            "§3",
            "a queued tone of the wrong type",
            |f| rep(f, "tokens.kdl", "    scrim", "    tone.queued 5\n    scrim"),
            TokenValue,
            Skin,
            None,
        ),
        case(
            "§3",
            "an easing that is a number",
            |f| rep(f, "tokens.kdl", "    scrim", "    easing.out 5\n    scrim"),
            TokenValue,
            Skin,
            None,
        ),
        case(
            "§3",
            "a motion token that is a colour",
            |f| {
                rep(
                    f,
                    "tokens.kdl",
                    "motion.fast 120",
                    "motion.fast \"#ffffff\"",
                )
            },
            TokenValue,
            Skin,
            None,
        ),
        case(
            "§3 §4",
            "a family token that is not a bundled family",
            |f| {
                rep(
                    f,
                    "tokens.kdl",
                    "    scrim",
                    "    easing.pick \"ease\"\n    scrim",
                );
                style(f, "family @easing.pick");
            },
            FontFamily,
            Skin,
            Some("@easing.pick"),
        ),
    ];
    v.extend(contract_cases());
    v.extend(small_rule_cases());
    v
}

/// Design 26 §3.1 and S10, design 27 §1, §3, §4 and §8: the current route on a nav
/// button, licence files, the pixel cap and the token forms.
fn small_rule_cases() -> Vec<Case> {
    use At::*;
    use Code::*;
    vec![
        case(
            "§5.3 §7",
            "on= on a button that is not testable",
            |f| {
                rep(
                    f,
                    SHELL,
                    "args=\"library\" on=\"{route.current == library}\"",
                    "args=\"library\" on=\"{library.stats.count}\"",
                )
            },
            TypeMismatch,
            Route("shell"),
            Some("\"{library.stats.count}\""),
        ),
        case(
            "§7 names",
            "route.current against a word that is no route",
            |f| {
                rep(
                    f,
                    SHELL,
                    "route.current == library",
                    "route.current == libary",
                )
            },
            UnknownName,
            Route("shell"),
            Some("libary"),
        ),
        case(
            "§1 §8 S10",
            "a licence file over 64 KiB",
            |f| add(f, "LICENSE", vec![b'x'; 64 * 1024 + 1]),
            LimitFile,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "a licence file that is not UTF-8",
            |f| add(f, "assets/OFL.txt", vec![0xff, 0xfe, 0xfd]),
            AssetText,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "a .txt where licence files are not allowed",
            |f| add(f, "routes/notes.txt", "x"),
            FileType,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "a LICENSE in routes",
            |f| add(f, "routes/LICENSE", "x"),
            FileType,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "a .txt in components",
            |f| add(f, "components/x.txt", "x"),
            FileType,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "a LICENSE.kdl is no licence file",
            |f| add(f, "LICENSE.kdl", "x"),
            FileType,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "assets/license.png is handled as a PNG",
            |f| add(f, "assets/license.png", "not a png"),
            AssetPng,
            Skin,
            None,
        ),
        case(
            "§1 S10",
            "assets/x.txt.png is handled as a PNG",
            |f| add(f, "assets/x.txt.png", "not a png"),
            AssetPng,
            Skin,
            None,
        ),
        case(
            "§3",
            "elevation given text",
            |f| rep(f, "tokens.kdl", "elevation.1 1", "elevation.1 \"high\""),
            TokenValue,
            Skin,
            None,
        ),
        case(
            "§1 §8",
            "a png 4097 px wide",
            |f| add(f, "assets/a.png", png_sized(4097, 1)),
            AssetPng,
            Skin,
            None,
        ),
        case(
            "§1 §8",
            "a png of 4096 by 4096, over 16 Mpx",
            |f| add(f, "assets/a.png", png_sized(4096, 4096)),
            AssetPng,
            Skin,
            None,
        ),
        case(
            "§3",
            "an easing outside the set",
            |f| {
                rep(
                    f,
                    "tokens.kdl",
                    "    scrim",
                    "    easing.out \"bounce\"\n    scrim",
                )
            },
            TokenValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "line-height under 0.5",
            |f| style(f, "line-height 0.4"),
            PropertyValue,
            Skin,
            None,
        ),
        case(
            "§4",
            "line-height over 4",
            |f| style(f, "line-height 4.1"),
            PropertyValue,
            Skin,
            None,
        ),
    ]
}

/// The contract check (design 27 §7, §9 step 2; design 26 S5, S16, S21): one failing
/// skin per rule. Each fires its own code and nothing else.
fn contract_cases() -> Vec<Case> {
    use At::*;
    use Code::*;
    vec![
        // §5.3 and S16: actions and their arguments.
        case(
            "§5.3 S16",
            "action without a required argument",
            |f| {
                rep(
                    f,
                    GAME,
                    "action=\"game.verify\" args=\"{$id}\"",
                    "action=\"game.verify\"",
                )
            },
            ActionArgs,
            Route("game"),
            Some("game.verify"),
        ),
        case(
            "§5.3 S16",
            "action argument the action has not got",
            |f| {
                rep(
                    f,
                    GAME,
                    "action=\"game.verify\" args=\"{$id}\"",
                    "action=\"game.verify\" args=\"{$id} fast=1\"",
                )
            },
            ActionArgs,
            Route("game"),
            Some("fast=1"),
        ),
        case(
            "§5.3 S16",
            "more arguments than the action takes",
            |f| {
                rep(
                    f,
                    GAME,
                    "action=\"game.verify\" args=\"{$id}\"",
                    "action=\"game.verify\" args=\"{$id} 3\"",
                )
            },
            ActionArgs,
            Route("game"),
            Some("3"),
        ),
        case(
            "§5.3 S16",
            "a parameter given twice",
            |f| {
                rep(
                    f,
                    GAME,
                    "action=\"game.verify\" args=\"{$id}\"",
                    "action=\"game.verify\" args=\"id={$id} id={$id}\"",
                )
            },
            ActionArgs,
            Route("game"),
            Some("id={$id}"),
        ),
        case(
            "§5.3 S16",
            "args without an action",
            |f| {
                rep(
                    f,
                    GAME,
                    "action=\"game.verify\" args=\"{$id}\"",
                    "args=\"{$id}\"",
                )
            },
            ActionArgs,
            Route("game"),
            Some("args"),
        ),
        case(
            "§5.3 S16",
            "a widget's action without its arguments",
            |f| {
                rep(
                    f,
                    GAME,
                    "for=\"game.update\" args=\"{$id}\"",
                    "for=\"game.update\"",
                )
            },
            ActionArgs,
            Route("game"),
            Some("game.update"),
        ),
        case(
            "§6.2 S16",
            "nav.go to a route the contract hasn't got",
            |f| rep(f, SHELL, "args=\"library\"", "args=\"libary\""),
            UnknownRoute,
            Route("shell"),
            Some("libary"),
        ),
        case(
            "§6.2 S16",
            "nav.go without a route's required parameter",
            |f| rep(f, SHELL, "args=\"library\"", "args=\"game\""),
            ActionArgs,
            Route("shell"),
            Some("\"game\""),
        ),
        case(
            "§6.2 S16",
            "nav.go to a section the route hasn't got",
            |f| {
                rep(
                    f,
                    GAME,
                    "args=\"game {$id} files\"",
                    "args=\"game {$id} fils\"",
                )
            },
            UnknownSection,
            Route("game"),
            Some("fils"),
        ),
        case(
            "§7",
            "a route name that route.current never is",
            |f| rep(f, SHELL, "current == library}", "current == ready}"),
            TypeMismatch,
            Route("shell"),
            Some("\"{route.current == ready}\""),
        ),
        case(
            "§7",
            "a quoted route name that is misspelt",
            |f| rep(f, SHELL, "current == library}", "current == 'libary'}"),
            TypeMismatch,
            Route("shell"),
            Some("\"{route.current == 'libary'}\""),
        ),
        case(
            "§7",
            "a number compared with a word",
            |f| {
                rep(
                    f,
                    SHELL,
                    "route.current == library}",
                    "library.stats.count == queue}",
                )
            },
            TypeMismatch,
            Route("shell"),
            Some("\"{library.stats.count == queue}\""),
        ),
        case(
            "§7",
            "a ui enum compared with another variable's word",
            |f| game_insert(f, "text \"x\" when=\"{ui.view == ready}\""),
            TypeMismatch,
            Route("game"),
            Some("\"{ui.view == ready}\""),
        ),
        case(
            "§7",
            "a word that is a source, in a where over unknown items",
            |f| {
                append(
                    f,
                    "components/game.kdl",
                    "\ncomponent \"words\" params=\"rows\" {\n    text \"{rows | where kind == queue | len}\"\n}\n",
                )
            },
            UnknownName,
            Skin,
            Some("queue"),
        ),
        case(
            "§5.3 S16",
            "a route that is not written out",
            |f| rep(f, SHELL, "args=\"library\"", "args=\"{app.version}\""),
            TypeMismatch,
            Route("shell"),
            Some("{app.version}"),
        ),
        case(
            "§5.3 S16",
            "tabs with a section the route hasn't got",
            |f| {
                rep(
                    f,
                    GAME,
                    "sections=\"overview builds dlc",
                    "sections=\"overview builds dlcs",
                )
            },
            UnknownSection,
            Route("game"),
            Some("dlcs"),
        ),
        case(
            "§5.3 S16",
            "tabs bound to a variable the skin hasn't got",
            |f| rep(f, GAME, "bind=\"ui.tab\"", "bind=\"ui.tabb\""),
            UnknownField,
            Route("game"),
            Some("ui.tabb"),
        ),
        case(
            "§5.3 S16",
            "tabs bound to a variable that isn't a section",
            |f| rep(f, GAME, "bind=\"ui.tab\"", "bind=\"ui.view\""),
            TypeMismatch,
            Route("game"),
            Some("ui.view"),
        ),
        case(
            "§4.1 S16",
            "ui.set of a variable the skin hasn't declared",
            |f| {
                rep(
                    f,
                    GAME,
                    "link-args=\"tab builds\"",
                    "link-args=\"tabb builds\"",
                )
            },
            ActionArgs,
            Route("game"),
            Some("tabb"),
        ),
        case(
            "§4.1 S16",
            "ui.set to a value the variable doesn't take",
            |f| {
                rep(
                    f,
                    GAME,
                    "link-args=\"tab builds\"",
                    "link-args=\"tab buildz\"",
                )
            },
            TypeMismatch,
            Route("game"),
            Some("buildz"),
        ),
        case(
            "§4.3 S16",
            "a word the parameter doesn't take",
            |f| rep(f, GAME, "args=\"{$id} game\"", "args=\"{$id} gam\""),
            TypeMismatch,
            Route("game"),
            Some("gam"),
        ),
        case(
            "§4.3 S16",
            "a field of the wrong type for an action parameter",
            |f| {
                rep(
                    f,
                    GAME,
                    "action=\"game.verify\" args=\"{$id}\"",
                    "action=\"game.verify\" args=\"{g.title}\"",
                )
            },
            TypeMismatch,
            Route("game"),
            Some("{g.title}"),
        ),
        // §5.5 and S5: app widgets.
        case(
            "§5.5 S16",
            "an unknown part",
            |f| rep(f, GAME, "part \"frame\"", "part \"frmae\""),
            UnknownPart,
            Route("game"),
            Some("frmae"),
        ),
        case(
            "§5.5 S16",
            "a variant of another widget",
            |f| {
                rep(
                    f,
                    GAME,
                    "row-actions=\"menu\"",
                    "row-actions=\"menu\" layout=\"stacked\"",
                )
            },
            UnknownProperty,
            Route("game"),
            Some("layout"),
        ),
        case(
            "§5.5 S16",
            "a variant value of another widget",
            |f| rep(f, GAME, "layout=\"stacked\"", "layout=\"list\""),
            UnknownVariant,
            Route("game"),
            Some("list"),
        ),
        case(
            "§5.5 S16",
            "a placement the widget doesn't take",
            |f| rep(f, GAME, "placement=\"inline\"", "placement=\"floating\""),
            UnknownVariant,
            Route("game"),
            Some("floating"),
        ),
        case(
            "§5.5 S5",
            "hiding the plan card's footnote",
            |f| {
                rep(
                    f,
                    GAME,
                    "part \"frame\" style=\"plan-frame\"",
                    "part \"footnote\" hidden=#true",
                )
            },
            HiddenPart,
            Route("game"),
            Some("#true"),
        ),
        case(
            "§5.5 S5",
            "hiding the plan card's confirm button",
            |f| {
                rep(
                    f,
                    GAME,
                    "part \"confirm\" style=\"plan-confirm\"",
                    "part \"confirm\" hidden=#true",
                )
            },
            HiddenPart,
            Route("game"),
            Some("#true"),
        ),
        // §4.9 and S21: settings.
        case(
            "§4.9 S21",
            "settings.set of an app-only key",
            |f| {
                rep(
                    f,
                    SHELL,
                    "action=\"nav.go\" args=\"saves\"",
                    "action=\"settings.set\" args=\"cloud_saves.writes on\"",
                )
            },
            SettingKey,
            Route("shell"),
            Some("cloud_saves.writes"),
        ),
        case(
            "§4.9 S21",
            "settings.set of an auth key",
            |f| {
                rep(
                    f,
                    SHELL,
                    "action=\"nav.go\" args=\"saves\"",
                    "action=\"settings.set\" args=\"auth.token_store file\"",
                )
            },
            SettingKey,
            Route("shell"),
            Some("auth.token_store"),
        ),
        case(
            "§4.9 S21",
            "settings.set of a key that is not listed",
            |f| {
                rep(
                    f,
                    SHELL,
                    "action=\"nav.go\" args=\"saves\"",
                    "action=\"settings.set\" args=\"download.workers 8\"",
                )
            },
            SettingKey,
            Route("shell"),
            Some("download.workers"),
        ),
        case(
            "§4.9 S21",
            "settings.set with a key read from a field",
            |f| {
                rep(
                    f,
                    SHELL,
                    "action=\"nav.go\" args=\"saves\"",
                    "action=\"settings.set\" args=\"{account.username} on\"",
                )
            },
            SettingKey,
            Route("shell"),
            Some("{account.username}"),
        ),
        case(
            "§4.9 S21",
            "settings.set with a key built from a template",
            |f| {
                rep(
                    f,
                    SHELL,
                    "action=\"nav.go\" args=\"saves\"",
                    "action=\"settings.set\" args=\"gui.{app.os} on\"",
                )
            },
            SettingKey,
            Route("shell"),
            Some("gui.{app.os}"),
        ),
        case(
            "§4.9 S21",
            "settings.set with the key by name from a template",
            |f| {
                rep(
                    f,
                    SHELL,
                    "action=\"nav.go\" args=\"saves\"",
                    "action=\"settings.set\" args=\"value={account.username} key={app.version}\"",
                )
            },
            SettingKey,
            Route("shell"),
            Some("{app.version}"),
        ),
        // §7: names.
        case(
            "§7 names",
            "an unknown name",
            |f| rep(f, GAME, "when=\"{g.runs_via}\"", "when=\"{runs}\""),
            UnknownName,
            Route("game"),
            Some("runs"),
        ),
        case(
            "§7 names",
            "a source named without its arguments",
            |f| rep(f, GAME, "when=\"{g.runs_via}\"", "when=\"{game.title}\""),
            UnknownName,
            Route("game"),
            Some("game"),
        ),
        case(
            "§7 names",
            "a name outside the predicate's item",
            |f| {
                rep(
                    f,
                    GAME,
                    "where installed == false",
                    "where instaled == false",
                )
            },
            UnknownName,
            Route("game"),
            Some("instaled"),
        ),
        case(
            "§7 names",
            "an each variable used after its block",
            |f| rep(f, GAME, "    tabs ", "    text \"{s.label}\"\n    tabs "),
            UnknownName,
            Route("game"),
            Some("s"),
        ),
        // §7: field paths from each kind of root.
        case(
            "§7 fields",
            "an unknown field of a let",
            |f| {
                rep(
                    f,
                    GAME,
                    "text \"{g.title}\" style=\"h1\"",
                    "text \"{g.titel}\" style=\"h1\"",
                )
            },
            UnknownField,
            Route("game"),
            Some(".titel"),
        ),
        case(
            "§7 fields",
            "an unknown field of a source call",
            |f| {
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    text \"{game($id).titel}\"\n    tabs ",
                )
            },
            UnknownField,
            Route("game"),
            Some(".titel"),
        ),
        case(
            "§7 fields",
            "an unknown field of an each item",
            |f| rep(f, GAME, "{s.label}", "{s.lable}"),
            UnknownField,
            Route("game"),
            Some(".lable"),
        ),
        case(
            "§7 fields",
            "an unknown field of a data-table row",
            |f| rep(f, GAME, "{row.version}", "{row.verzion}"),
            UnknownField,
            Route("game"),
            Some(".verzion"),
        ),
        case(
            "§7 fields",
            "an unknown field of ui",
            |f| rep(f, GAME, "switch \"{ui.tab}\"", "switch \"{ui.tabb}\""),
            UnknownField,
            Route("game"),
            Some(".tabb"),
        ),
        case(
            "§7 fields",
            "an unknown field of app",
            |f| {
                rep(
                    f,
                    SHELL,
                    "        outlet",
                    "        text \"{app.versoin}\"\n        outlet",
                )
            },
            UnknownField,
            Route("shell"),
            Some(".versoin"),
        ),
        case(
            "§7 fields",
            "an unknown field of a source that has no arguments",
            |f| rep(f, SHELL, "{account.username}", "{account.usernam}"),
            UnknownField,
            Route("shell"),
            Some(".usernam"),
        ),
        case(
            "§7 fields",
            "an unknown field of a source named with a dot",
            |f| rep(f, SHELL, "{library.stats.count}", "{library.stats.cnt}"),
            UnknownField,
            Route("shell"),
            Some(".cnt"),
        ),
        case(
            "§7 fields",
            "an unknown field deep in a path",
            |f| rep(f, GAME, "{g.art.cover}", "{g.art.covr}"),
            UnknownField,
            Route("game"),
            Some(".covr"),
        ),
        case(
            "§7 fields",
            "a field of a value that has none",
            |f| {
                rep(
                    f,
                    GAME,
                    "{g.title}\" style=\"h1\"",
                    "{g.title.size}\" style=\"h1\"",
                )
            },
            UnknownField,
            Route("game"),
            Some(".size"),
        ),
        case(
            "§7 fields",
            "a field of an opaque value",
            |f| {
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    text \"{plan.preview('game.update', $id).x}\"\n    tabs ",
                )
            },
            UnknownField,
            Route("game"),
            Some(".x"),
        ),
        case(
            "§7 fields",
            "an unknown sort key",
            |f| rep(f, GAME, "sort -date | take 2", "sort -dat | take 2"),
            UnknownField,
            Route("game"),
            Some("\"{snaps.items | sort -dat | take 2}\""),
        ),
        case(
            "§7 fields",
            "a field path checked at each use of a component",
            |f| {
                append(
                    f,
                    "components/stock.kdl",
                    "\ncomponent \"who\" params=\"g\" { text \"{g.title}\" }\n",
                );
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    use \"who\" g=\"{game($id)}\"\n    use \"who\" g=\"{account}\"\n    tabs ",
                )
            },
            UnknownField,
            Route("game"),
            Some("\"{account}\""),
        ),
        // §7: sources.
        case(
            "§7 S16",
            "a source the contract hasn't got",
            |f| {
                rep(
                    f,
                    GAME,
                    "{game.builds($id) | take 4}",
                    "{game.buidls($id) | take 4}",
                )
            },
            UnknownSource,
            Route("game"),
            Some("game.buidls($id)"),
        ),
        case(
            "§7 S16",
            "a source that is only a widget's",
            |f| {
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    text \"{job.log($id)}\"\n    tabs ",
                )
            },
            UnknownSource,
            Route("game"),
            Some("job.log($id)"),
        ),
        case(
            "§7 S16",
            "a source with the wrong number of arguments",
            |f| {
                rep(
                    f,
                    GAME,
                    "{game.builds($id) | take 4}",
                    "{game.builds($id, 1, 2) | take 4}",
                )
            },
            SourceArgs,
            Route("game"),
            Some("game.builds($id, 1, 2)"),
        ),
        // §7: types where an element needs one.
        case(
            "§7 types",
            "when on text",
            |f| rep(f, GAME, "when=\"{g.runs_via}\"", "when=\"{g.title}\""),
            TypeMismatch,
            Route("game"),
            Some("\"{g.title}\""),
        ),
        case(
            "§7 types",
            "on with a number",
            |f| rep(f, GAME, "on=\"{snaps.auto}\"", "on=\"{g.year}\""),
            TypeMismatch,
            Route("game"),
            Some("\"{g.year}\""),
        ),
        case(
            "§7 types",
            "each over something that is not a list",
            |f| {
                rep(
                    f,
                    GAME,
                    "in=\"{snaps.items | sort -date | take 2}\"",
                    "in=\"{snaps}\"",
                )
            },
            TypeMismatch,
            Route("game"),
            Some("\"{snaps}\""),
        ),
        case(
            "§7 types",
            "rows that are not a list",
            |f| {
                rep(
                    f,
                    GAME,
                    "rows=\"{game.builds($id) | take 4}\"",
                    "rows=\"{g}\"",
                )
            },
            TypeMismatch,
            Route("game"),
            Some("\"{g}\""),
        ),
        case(
            "§7 types",
            "progress that is not a fraction",
            |f| {
                rep(
                    f,
                    SHELL,
                    "progress=\"{q.current.fraction}\"",
                    "progress=\"{q.queued}\"",
                )
            },
            TypeMismatch,
            Route("shell"),
            Some("\"{q.queued}\""),
        ),
        case(
            "§7 types",
            "a parameter's type checked at the use",
            |f| {
                append(
                    f,
                    "components/stock.kdl",
                    "\ncomponent \"each-of\" params=\"xs\" { each \"x\" in=\"{xs}\" { text \"{x}\" } }\n",
                );
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    use \"each-of\" xs=\"{g.title}\"\n    use \"each-of\" xs=\"{game.builds($id)}\"\n    tabs ",
                )
            },
            TypeMismatch,
            Route("game"),
            Some("\"{g.title}\""),
        ),
        case(
            "§7 types",
            "a parameter used as a progress, given text at the use",
            |f| {
                append(
                    f,
                    "components/stock.kdl",
                    "\ncomponent \"bar-of\" params=\"p\" { box style=\"bar-fill\" progress=\"{p}\" }\n",
                );
                rep(
                    f,
                    GAME,
                    "    tabs ",
                    "    use \"bar-of\" p=\"{g.title}\"\n    tabs ",
                )
            },
            TypeMismatch,
            Route("game"),
            Some("\"{g.title}\""),
        ),
        // A component passes the action's name: each `use` is checked against the
        // contract (S16).
        case(
            "§5.7 S16",
            "a component given an action the contract hasn't got",
            |f| {
                game_insert(
                    f,
                    "use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"game.verfy\" args=\"{$id}\"",
                )
            },
            UnknownAction,
            Route("game"),
            Some("\"game.verfy\""),
        ),
        case(
            "§5.7 S16",
            "a component given an action without all its arguments",
            |f| {
                game_insert(
                    f,
                    "use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"dlc.add\" args=\"{$id}\"",
                )
            },
            ActionArgs,
            Route("game"),
            Some("\"{$id}\""),
        ),
        case(
            "§5.7 S16",
            "a component given a route that isn't one",
            |f| {
                game_insert(
                    f,
                    "use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"nav.go\" args=\"libary\"",
                )
            },
            UnknownRoute,
            Route("game"),
            Some("\"libary\""),
        ),
    ]
}

fn findings(r: Result<SkinPackage, gogo_skin::Report>) -> Vec<gogo_skin::Finding> {
    match r {
        Ok(p) => p.report().findings().to_vec(),
        Err(r) => r.findings().to_vec(),
    }
}

#[test]
fn every_rule_reports_its_code_where_it_belongs() {
    let mut bad = Vec::new();
    for c in cases() {
        let mut files = base();
        (c.edit)(&mut files);
        let all = findings(load(files));
        let Some(f) = all.iter().find(|f| f.code == c.code) else {
            let got: Vec<String> = all
                .iter()
                .map(|f| format!("{} {:?} {:?}", f.code, f.text, f.message))
                .collect();
            bad.push(format!(
                "{} ({}): no {}; got {got:?}",
                c.name, c.section, c.code
            ));
            continue;
        };
        let fired: std::collections::BTreeSet<Code> = all.iter().map(|f| f.code).collect();
        if fired != std::collections::BTreeSet::from([c.code]) {
            bad.push(format!(
                "{} ({}): expected only {}, got {:?}",
                c.name,
                c.section,
                c.code,
                fired.iter().map(|c| c.as_str()).collect::<Vec<_>>()
            ));
        }
        let level_ok = match (&f.level, c.at) {
            (Level::Skin, At::Skin) | (Level::Warning, At::Warn) => true,
            (Level::Route(r), At::Route(want)) => r == want,
            _ => false,
        };
        if !level_ok {
            bad.push(format!(
                "{} ({}): {} is {:?}, wanted {:?}",
                c.name, c.section, c.code, f.level, c.at
            ));
        }
        if let Some(want) = c.text
            && f.text.as_deref() != Some(want)
        {
            bad.push(format!(
                "{} ({}): span covers {:?}, wanted {want:?}",
                c.name, c.section, f.text
            ));
        }
        if c.text.is_none() && c.at == At::Skin && f.span.is_some() && f.text.is_none() {
            bad.push(format!("{}: span points outside the file", c.name));
        }
    }
    assert!(
        bad.is_empty(),
        "{} case(s) failed:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn every_code_has_a_case_or_a_reason() {
    // Codes no table case can reach on its own: they need a folder on disk (symlinks,
    // read errors), covered in `load_dir.rs`.
    let on_disk = [Code::Symlink, Code::Io];
    let covered: std::collections::HashSet<Code> = cases().iter().map(|c| c.code).collect();
    let missing: Vec<_> = Code::ALL
        .iter()
        .filter(|c| !covered.contains(c) && !on_disk.contains(c))
        .collect();
    assert!(missing.is_empty(), "no failing fixture for {missing:?}");
}

#[test]
fn a_route_error_drops_the_route_and_keeps_the_skin() {
    let mut files = base();
    rep(&mut files, GAME, "icon=\"play\"", "icon=\"plai\"");
    let pkg = load(files).expect("a route error does not fail the skin");
    assert!(pkg.routes.contains_key("shell"));
    assert!(
        !pkg.routes.contains_key("game"),
        "the failed route is not offered"
    );
    assert_eq!(
        pkg.report().failed_routes().into_iter().collect::<Vec<_>>(),
        ["game"]
    );
    assert!(!pkg.report().has_skin_errors());
}

#[test]
fn a_skin_error_fails_the_skin() {
    let mut files = base();
    rep(&mut files, "tokens.kdl", "    space.4 16\n", "");
    let report = load(files).expect_err("a missing token fails the skin");
    assert!(report.has_skin_errors());
    assert!(report.errors().count() >= 1);
}

#[test]
fn assets_that_are_valid_are_accepted_and_referenced() {
    let mut files = base();
    add(&mut files, "assets/logo.png", png());
    add(&mut files, "assets/icons/mark.svg", SVG);
    add(&mut files, "assets/fonts/extra.otf", font());
    rep(
        &mut files,
        GAME,
        "    tabs ",
        "    image src=\"assets/logo.png\" label=\"Logo\"\n    image src=\"assets/icons/mark.svg\" label=\"Mark\"\n    tabs ",
    );
    let pkg = load(files).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
    assert_eq!(pkg.assets.len(), 3);
    assert_eq!(pkg.assets["assets/logo.png"].size, Some((1, 1)));
}

#[test]
fn effects_and_stretch_forms_are_accepted() {
    let mut files = base();
    append(
        &mut files,
        "styles.kdl",
        r##"
style "fx" {
    shadow 0 6 18 "#00000066"
    shadow 0 1 2 "#000000" 3
    inner-shadow 0 1 4 "#00000088"
    glow 6 "{@accent alpha=.8}" 18 "{@accent alpha=.35}"
    bloom-text 8 "{@accent alpha=.6}"
    noise 0.06
    scanlines 3 0.18
    reflection 120 0.14
    blur-behind 12
    gradient linear 180 @bg1 @bg0
    fill transparent
    border 1 transparent
    stretch 90
    font @font.title
    :hover { gradient radial @bg1 @bg0 "#00000000"; effects none }
    :on { shadow none; bloom-text none }
}
style "wide" { noise 0.06 7; family "Noto Sans"; stretch 87.5; padding 1 2; margin -4 0 1 2; radius 1 2 3 4 }
"##,
    );
    let pkg = load(files).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
    assert_eq!(pkg.styles["fx"].states.len(), 2);
}

#[test]
fn template_and_expression_errors_point_inside_the_string() {
    let mut files = base();
    rep(
        &mut files,
        GAME,
        "{g.title}\" style=\"crumb\"",
        "{g.title + }\" style=\"crumb\"",
    );
    let all = findings(load(files));
    let f = all
        .iter()
        .find(|f| f.code == Code::Expression)
        .expect("expression error");
    let text = f.text.as_deref().unwrap_or_default();
    assert!(
        text.len() <= 3,
        "the span is a token, not the whole string: {text:?}"
    );
    assert!(f.location.starts_with("routes/game.kdl:"), "{}", f.location);
}

#[test]
fn route_files_not_listed_are_a_warning() {
    let mut files = base();
    rep(&mut files, "skin.kdl", " \"queue\"", "");
    let pkg = load(files).expect("loads");
    let w: Vec<_> = pkg.report().warnings().collect();
    assert_eq!(w.len(), 1, "{}", pkg.report());
    assert_eq!(w[0].code, Code::RouteFile);
    assert!(!pkg.routes.contains_key("queue"));
}

#[test]
fn findings_are_miette_diagnostics_with_codes_and_help() {
    use miette::Diagnostic;
    let mut files = base();
    rep(
        &mut files,
        GAME,
        "action=\"game.verify\"",
        "action=\"game.verfy\"",
    );
    let all = findings(load(files));
    let f = all
        .iter()
        .find(|f| f.code == Code::UnknownAction)
        .expect("finding");
    assert_eq!(
        f.code().expect("a code").to_string(),
        "gogo_skin::unknown_action"
    );
    assert!(
        f.help().is_some(),
        "every finding says how to fix it where it can"
    );
    assert!(f.source_code().is_some());
    assert!(f.labels().is_some_and(|mut l| l.next().is_some()));
    assert_eq!(f.severity(), Some(miette::Severity::Error));
}

fn many_warnings() -> String {
    let mut many = String::new();
    for i in 0..260 {
        many.push_str(&format!(
            "\nstyle \"w{i}\" {{ opacity 0.5; opacity 0.6 }}\n"
        ));
    }
    many
}

#[test]
fn warnings_never_hide_a_route_error() {
    let mut files = base();
    append(&mut files, "styles.kdl", &many_warnings());
    rep(&mut files, GAME, "    tabs ", "    bogus\n    tabs ");
    let pkg = load(files).expect("a route error does not fail the skin");
    let report = pkg.report();
    assert!(report.suppressed() > 0, "the warnings overflowed the cap");
    assert_eq!(report.warning_total(), 260);
    assert_eq!(report.error_total(), 1);
    assert_eq!(report.errors().count(), 1, "the error is stored");
    assert_eq!(
        report.failed_routes().into_iter().collect::<Vec<_>>(),
        ["game"]
    );
    assert!(!pkg.routes.contains_key("game"));
    assert!(pkg.routes.contains_key("shell"));
    assert!(!report.has_skin_errors());
}

#[test]
fn warnings_never_hide_a_skin_error() {
    let mut files = base();
    append(&mut files, "styles.kdl", &many_warnings());
    append(&mut files, "components/stock.kdl", "\nbogus\n");
    let report = load(files).expect_err("a component error fails the skin");
    assert!(report.has_skin_errors());
    assert!(report.errors().count() >= 1);
    assert!(report.suppressed() > 0);
}

#[test]
fn errors_past_the_cap_still_fail_the_skin() {
    let mut files = base();
    let mut many = String::new();
    for i in 0..300 {
        many.push_str(&format!("\nstyle \"e{i}\" {{ opacity 9 }}\n"));
    }
    append(&mut files, "styles.kdl", &many);
    let report = load(files).expect_err("style errors fail the skin");
    assert_eq!(report.error_total(), 300);
    assert!(report.errors().count() < 300, "storage is capped");
    assert!(report.has_skin_errors());
}

#[test]
fn the_queued_and_paused_tones_are_optional() {
    // The default skin defines neither; adding them is accepted too.
    let pkg = load(base()).expect("loads without them");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
    let mut files = base();
    rep(
        &mut files,
        "tokens.kdl",
        "    scrim",
        "    tone.queued \"@text-lo\"\n    tone.paused \"@warn\"\n    scrim",
    );
    let pkg = load(files).expect("loads with them");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
}

#[test]
fn a_component_may_pass_its_action_parameter_on() {
    let mut files = base();
    game_insert(
        &mut files,
        "use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"game.verify\" args=\"{$id}\"",
    );
    let pkg = load(files).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
}

// ---- the contract check: what it accepts, what it keeps, and the S21 paths ----

/// The route nodes whose name is `name`, anywhere in the tree.
fn find<'a>(nodes: &'a [gogo_skin::Node], name: &str, out: &mut Vec<&'a gogo_skin::Node>) {
    for n in nodes {
        if n.name == name {
            out.push(n);
        }
        find(&n.children, name, out);
    }
}

#[test]
fn what_the_contract_allows_loads_clean() {
    let mut files = base();
    // An optional part hidden, a skin-settable key, a status and an OS as bare words, a
    // `where` over a value of unknown type, an optional route parameter by name, and a
    // section by name.
    rep(
        &mut files,
        GAME,
        "part \"badge\" style=\"plan-badge\"",
        "part \"badge\" hidden=#true",
    );
    game_insert(
        &mut files,
        "button \"Dark\" action=\"settings.set\" args=\"gui.mode dark\"",
    );
    game_insert(
        &mut files,
        "text \"{g.status == setting_up and app.os == linux ? 'a' : 'b'}\"",
    );
    game_insert(&mut files, "text \"{$id | where whatever | len}\"");
    game_insert(
        &mut files,
        "button \"Section\" action=\"nav.go\" args=\"game id={$id} section=builds\"",
    );
    let pkg = load(files).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
}

#[test]
fn a_component_may_be_passed_an_action_that_fits() {
    let mut files = base();
    game_insert(
        &mut files,
        "use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"game.verify\" args=\"{$id}\"",
    );
    game_insert(
        &mut files,
        "use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"nav.go\" args=\"library\"",
    );
    let pkg = load(files).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
}

#[test]
fn a_settings_key_passed_through_a_component_is_checked_at_the_use() {
    // S21: the key reaches `settings.set` through a parameter, and is checked as the text
    // the `use` gave. The single forwarded argument leaves `value` out, so that is an
    // argument error as well.
    let mut files = base();
    game_insert(
        &mut files,
        "use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"settings.set\" args=\"cloud_saves.writes\"",
    );
    let all = findings(load(files));
    let codes: std::collections::BTreeSet<Code> = all.iter().map(|f| f.code).collect();
    assert_eq!(
        codes,
        std::collections::BTreeSet::from([Code::SettingKey, Code::ActionArgs]),
        "{all:?}"
    );
    let key = all.iter().find(|f| f.code == Code::SettingKey).unwrap();
    assert_eq!(key.level, Level::Route("game".into()));
    assert_eq!(key.text.as_deref(), Some("\"cloud_saves.writes\""));
    assert!(key.message.contains("app-only"), "{}", key.message);

    // And one that is not a literal at the use can't be checked, so it is refused.
    let mut files = base();
    game_insert(
        &mut files,
        "use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"settings.set\" args=\"{account.username}\"",
    );
    let all = findings(load(files));
    let key = all
        .iter()
        .find(|f| f.code == Code::SettingKey)
        .expect("refused");
    assert_eq!(key.text.as_deref(), Some("\"{account.username}\""));
}

#[test]
fn resolved_specs_are_kept_in_the_tree() {
    use gogo_skin::ast::ArgRef;
    use gogo_skin::expr::{Expr, Template};
    let pkg = load(base()).expect("loads");
    let game = &pkg.routes["game"];

    let mut buttons = Vec::new();
    find(&game.children, "button", &mut buttons);
    let play = buttons
        .iter()
        .find(|b| b.plain_prop("action") == Some("game.play"))
        .expect("play");
    assert_eq!(play.resolved.actions.len(), 1);
    assert_eq!(play.resolved.actions[0].spec.name, "game.play");
    assert_eq!(play.resolved.actions[0].args[0].param.name, "id");

    // `nav.go args="game {$id} files"`: the route, then its parameters.
    let files = buttons
        .iter()
        .find(|b| b.plain_prop("args").is_none() && b.plain_arg(0) == Some("Download files"))
        .expect("files");
    let args = &files.resolved.actions[0].args;
    assert!(matches!(&args[0].value, ArgRef::Route(r) if r.name == "game"));
    assert!(matches!(&args[1].value, ArgRef::RouteParam(p) if p.name == "id"));
    assert!(matches!(&args[2].value, ArgRef::RouteParam(p) if p.name == "section"));

    let mut cards = Vec::new();
    find(&game.children, "plan-card", &mut cards);
    let card = cards[0];
    assert_eq!(card.resolved.widget.map(|w| w.name), Some("plan-card"));
    assert_eq!(card.resolved.actions[0].spec.name, "game.update");
    assert!(
        card.resolved
            .variants
            .iter()
            .any(|(v, x)| v.name == "layout" && *x == "stacked")
    );
    let mut parts = Vec::new();
    find(&card.children, "part", &mut parts);
    assert!(parts.iter().all(|p| p.resolved.part.is_some()));

    let mut tabs = Vec::new();
    find(&game.children, "tabs", &mut tabs);
    assert_eq!(tabs[0].resolved.sections.len(), 7);

    // Sources named in an expression hold their spec.
    let Some(let_) = game.children.iter().find(|n| n.name == "let") else {
        panic!("no let");
    };
    let Some(Template::Expr(Expr::Call { source, .. })) =
        let_.props.first().map(|p| match &p.arg.val {
            gogo_skin::Val::Text(t) => t.clone(),
            _ => panic!("not text"),
        })
    else {
        panic!("not a call");
    };
    assert_eq!(source.map(|s| s.name), Some("game"));
}

/// A component that rebinds one of its own parameters before forwarding it as an action
/// argument, then a `use` of it in the game route.
fn shadowed_use(component: &str, use_args: &str) -> Vec<Code> {
    let mut files = base();
    append(&mut files, "components/stock.kdl", component);
    game_insert(&mut files, &format!("use \"shadow\" {use_args}"));
    let mut v: Vec<Code> = findings(load(files)).iter().map(|f| f.code).collect();
    v.sort();
    v
}

#[test]
fn a_let_over_a_parameter_hides_what_the_use_gave() {
    // S21: `args` is a dynamic value in the body, whatever text the `use` passed.
    let codes = shadowed_use(
        "\ncomponent \"shadow\" params=\"action args\" {\n    let args=\"{game($id).slug}\"\n    button \"B\" action=\"{action}\" args=\"{args}\"\n}\n",
        "action=\"settings.set\" args=\"gui.mode\"",
    );
    // The key is dynamic at bind; the single forwarded argument leaves `value` out.
    assert_eq!(codes, vec![Code::ActionArgs, Code::SettingKey]);
}

#[test]
fn an_each_over_a_parameter_hides_what_the_use_gave() {
    let codes = shadowed_use(
        "\ncomponent \"shadow\" params=\"action args\" {\n    each \"args\" in=\"{game.saves($id) | take 1}\" {\n        button \"B\" action=\"{action}\" args=\"{args}\"\n    }\n}\n",
        "action=\"settings.set\" args=\"gui.mode\"",
    );
    // As above, and the item of the list is not a plain value.
    assert_eq!(
        codes,
        vec![Code::ActionArgs, Code::SettingKey, Code::TypeMismatch]
    );
}

#[test]
fn a_let_over_the_action_parameter_hides_the_action_the_use_gave() {
    // The action named at the `use` is a literal, the one in the body is a variable: it
    // is refused, not checked as `game.verify`.
    let codes = shadowed_use(
        "\ncomponent \"shadow\" params=\"action args\" {\n    let action=\"{game($id).slug}\"\n    button \"B\" action=\"{action}\" args=\"{args}\"\n}\n",
        "action=\"game.verify\" args=\"{$id}\"",
    );
    assert_eq!(codes, vec![Code::ActionDynamic]);
}

#[test]
fn a_parameter_tested_by_when_takes_any_type() {
    // A `use` may leave a parameter out, so it may be null, and `when` tests null: no
    // type given at the `use` is a mismatch there.
    let mut files = base();
    append(
        &mut files,
        "components/stock.kdl",
        "\ncomponent \"maybe\" params=\"p\" { text \"x\" when=\"{p}\" }\n",
    );
    game_insert(&mut files, "use \"maybe\" p=\"{g.title}\"");
    game_insert(&mut files, "use \"maybe\" p=\"{game.builds($id)}\"");
    let pkg = load(files).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
}

#[test]
fn licence_files_are_accepted_at_the_root_and_under_assets() {
    let mut files = base();
    for path in [
        "LICENSE",
        "OFL.txt",
        "LICENSE.md",
        "assets/LICENSE",
        "assets/OFL.txt",
        "assets/fonts/LICENSE-OFL",
        "assets/fonts/notes.TXT",
        "LICENCE",
        "assets/LICENCE-OFL.md",
    ] {
        add(&mut files, path, "SIL Open Font License");
    }
    // The size limit is inclusive, and a non-ASCII text is still text.
    add(&mut files, "assets/big.txt", vec![b'x'; 64 * 1024]);
    add(&mut files, "assets/utf8.txt", "Gro\u{df}e Schrift");
    let pkg = load(files).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
    assert!(pkg.assets.is_empty(), "a licence file is not an asset");
}

#[test]
fn a_png_at_the_pixel_cap_is_accepted() {
    let mut files = base();
    add(&mut files, "assets/a.png", png_sized(4096, 3906));
    add(&mut files, "assets/b.png", png_sized(4096, 1));
    let pkg = load(files).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
    assert_eq!(pkg.assets["assets/a.png"].size, Some((4096, 3906)));
}

#[test]
fn every_easing_word_and_the_line_height_bounds_are_accepted() {
    let mut files = base();
    let words = ["linear", "ease", "ease-in", "ease-out", "ease-in-out"];
    let tokens: String = words
        .iter()
        .map(|w| format!("    easing.{w} \"{w}\"\n"))
        .collect();
    rep_all(
        &mut files,
        "tokens.kdl",
        "    scrim",
        &format!("{tokens}    scrim"),
    );
    append(
        &mut files,
        "styles.kdl",
        "\nstyle \"lh\" { line-height 0.5 }\nstyle \"lh2\" { line-height 4 }\n",
    );
    let pkg = load(files).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
}

#[test]
fn route_current_compares_with_a_bare_or_a_quoted_route_word() {
    let mut files = base();
    rep(
        &mut files,
        SHELL,
        "route.current == library",
        "route.current == 'sign-in' or route.current == 'queue' or route.current == queue or route.current == game",
    );
    let pkg = load(files).expect("loads");
    assert!(pkg.report().is_clean(), "{}", pkg.report());
}

// ---- design 27 §5.7: a bare `{p}` in `args` stands for the terms the `use` wrote ----

const TILE: &str = "use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\"";

/// The codes the game route reports for one more `use`, sorted.
fn codes_of_use(node: &str) -> Vec<Code> {
    let mut files = base();
    game_insert(&mut files, node);
    let mut v: Vec<Code> = findings(load(files)).iter().map(|f| f.code).collect();
    v.sort();
    v
}

#[test]
fn a_forwarded_args_of_several_terms_is_checked_term_by_term() {
    // `dlc.add` takes `id` and `dlc`: both terms of the `use` land on them.
    assert_eq!(
        codes_of_use(&format!("{TILE} action=\"dlc.add\" args=\"{{$id}} all\"")),
        vec![]
    );
    // Named terms keep their names, so they may come in any order.
    assert_eq!(
        codes_of_use(&format!(
            "{TILE} action=\"dlc.add\" args=\"dlc=all id={{$id}}\""
        )),
        vec![]
    );
    // The second term is missing.
    assert_eq!(
        codes_of_use(&format!("{TILE} action=\"dlc.add\" args=\"{{$id}}\"")),
        vec![Code::ActionArgs]
    );
    // One term too many.
    assert_eq!(
        codes_of_use(&format!(
            "{TILE} action=\"dlc.add\" args=\"{{$id}} all extra\""
        )),
        vec![Code::ActionArgs]
    );
}

#[test]
fn a_spliced_settings_key_that_is_app_only_is_refused() {
    let mut files = base();
    game_insert(
        &mut files,
        &format!("{TILE} action=\"settings.set\" args=\"cloud_saves.writes on\""),
    );
    let all = findings(load(files));
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].code, Code::SettingKey);
    assert_eq!(all[0].level, Level::Route("game".into()));
    assert!(all[0].message.contains("app-only"), "{}", all[0].message);
    assert_eq!(all[0].text.as_deref(), Some("\"cloud_saves.writes on\""));
}

#[test]
fn a_spliced_settings_key_that_is_not_listed_is_refused() {
    // S21: a key that is on neither list is not a key a skin may set.
    let mut files = base();
    game_insert(
        &mut files,
        &format!("{TILE} action=\"settings.set\" args=\"gui.nonsense on\""),
    );
    let all = findings(load(files));
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].code, Code::SettingKey);
    assert!(!all[0].message.contains("app-only"), "{}", all[0].message);
    // A listed key goes through.
    assert_eq!(
        codes_of_use(&format!(
            "{TILE} action=\"settings.set\" args=\"gui.mode dark\""
        )),
        vec![]
    );
}

#[test]
fn a_spliced_term_of_the_wrong_type_is_refused() {
    // `id` is a game id; `g.playtime` is a number.
    assert_eq!(
        codes_of_use(&format!(
            "{TILE} action=\"dlc.add\" args=\"{{g.playtime}} all\""
        )),
        vec![Code::TypeMismatch]
    );
    // A word outside a fixed set, spliced from the `use`, is checked as well.
    assert_eq!(
        codes_of_use(&format!(
            "{TILE} action=\"game.open_folder\" args=\"{{$id}} attic\""
        )),
        vec![Code::TypeMismatch]
    );
}

#[test]
fn style_values_are_typed_once_at_load() {
    use gogo_skin::ast::Operand;
    let pkg = load(base()).expect("loads");
    // A style declaration: `padding 0 16` is two numbers.
    let button = &pkg.styles["button"];
    let padding = button.props.iter().find(|p| p.name == "padding").unwrap();
    assert_eq!(padding.operands, [Operand::Num(0.0), Operand::Num(16.0)]);
    // An inline property: a string is split and parsed, a token is named, an expression
    // is kept.
    let mut rows = Vec::new();
    find(&pkg.routes["game"].children, "row", &mut rows);
    let get = |name: &str| {
        rows.iter()
            .find_map(|r| r.props.iter().find(|p| p.name == name))
            .unwrap_or_else(|| panic!("no `{name}`"))
    };
    assert_eq!(
        get("margin").operands,
        [
            Operand::Num(6.0),
            Operand::Num(0.0),
            Operand::Num(0.0),
            Operand::Num(0.0)
        ]
    );
    assert_eq!(get("gap").operands, [Operand::Token("space.6".into())]);
    assert_eq!(get("align").operands, [Operand::Word("start".into())]);
    // Not a style property: nothing to type.
    assert!(get("id").operands.is_empty());
}

const OUTER: &str = "\ncomponent \"outer\" params=\"action args\" {\n    use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"{action}\" args=\"{args}\"\n}\n";

#[test]
fn a_component_may_pass_its_args_on_to_another_component() {
    // Two levels: `outer` hands `action` and `args` on to `summary-tile`. What the `use`
    // of `outer` wrote is still checked against the action.
    let run = |node: &str| {
        let mut files = base();
        append(&mut files, "components/stock.kdl", OUTER);
        game_insert(&mut files, node);
        let mut v: Vec<Code> = findings(load(files)).iter().map(|f| f.code).collect();
        v.sort();
        v
    };
    assert_eq!(
        run("use \"outer\" action=\"dlc.add\" args=\"{$id} all\""),
        vec![]
    );
    assert_eq!(
        run("use \"outer\" action=\"settings.set\" args=\"cloud_saves.writes on\""),
        vec![Code::SettingKey]
    );
    assert_eq!(
        run("use \"outer\" action=\"dlc.add\" args=\"{g.playtime} all\""),
        vec![Code::TypeMismatch]
    );
    assert_eq!(
        run("use \"outer\" action=\"dlc.add\" args=\"{$id}\""),
        vec![Code::ActionArgs]
    );
}

// ---- design 27 §5.7: what is spliced, and what stays dynamic ----

/// The findings of the game route with `components` added to the stock ones and one more
/// node, sorted by code.
fn spliced(components: &str, node: &str) -> Vec<gogo_skin::Finding> {
    let mut files = base();
    append(&mut files, "components/stock.kdl", components);
    game_insert(&mut files, node);
    let mut v = findings(load(files));
    v.sort_by_key(|f| f.code);
    v
}

fn codes(v: &[gogo_skin::Finding]) -> Vec<Code> {
    v.iter().map(|f| f.code).collect()
}

/// A component `name` whose one button gets its action and `args` from its parameters,
/// with `body` as the `args` it writes (`{args}` forwards the `use`'s terms).
fn forwarder(name: &str, params: &str, body: &str) -> String {
    format!(
        "\ncomponent \"{name}\" params=\"{params}\" {{\n    button \"B\" action=\"{{action}}\" args=\"{body}\"\n}}\n"
    )
}

#[test]
fn a_missing_forwarded_argument_is_reported_at_the_use() {
    let fixed = forwarder("fixed", "action args", "{args}");
    let all = spliced(&fixed, "use \"fixed\" action=\"dlc.add\" args=\"{$id}\"");
    assert_eq!(codes(&all), vec![Code::ActionArgs], "{all:?}");
    // The `args` of the `use`, not the component's own.
    assert_eq!(all[0].text.as_deref(), Some("\"{$id}\""));
    assert_eq!(all[0].level, Level::Route("game".into()));
    assert!(all[0].message.contains("needs `dlc`"), "{}", all[0].message);
    assert!(all[0].message.contains("in component `fixed`"));
    assert!(
        spliced(
            &fixed,
            "use \"fixed\" action=\"dlc.add\" args=\"{$id} all\""
        )
        .is_empty()
    );
    // A `nav.go` route parameter that is missing is reported there too.
    let go = forwarder("go", "action args", "{args}");
    let all = spliced(&go, "use \"go\" action=\"nav.go\" args=\"game\"");
    assert_eq!(codes(&all), vec![Code::ActionArgs], "{all:?}");
    assert_eq!(all[0].text.as_deref(), Some("\"game\""));
}

#[test]
fn only_a_bare_parameter_is_spliced() {
    // S21: a key reaches `settings.set` as a literal or not at all. A `{args}` that has
    // more around it stays dynamic, so the literal-only `key` rejects it, though the
    // `use` wrote a listed key.
    let run = |body: &str| {
        let comp = forwarder("setter", "action args", body);
        spliced(
            &comp,
            "use \"setter\" action=\"settings.set\" args=\"gui.mode\"",
        )
    };
    assert!(run("{args} dark").is_empty());
    assert!(run("key={args} value=dark").is_empty());
    for body in [
        "{args}x dark",
        "{args.x} dark",
        "\\\"{args} dark\\\" dark",
        "key={args}x value=dark",
    ] {
        let all = run(body);
        // Reported at the `use`, where the key was written.
        assert!(
            all.iter()
                .any(|f| f.code == Code::SettingKey && f.level == Level::Route("game".into())),
            "`{body}`: {all:?}"
        );
    }
}

#[test]
fn a_named_term_takes_one_unnamed_term_only() {
    let comp = forwarder("named", "action args", "id={args} dlc=all");
    let run = |given: &str| {
        spliced(
            &comp,
            &format!("use \"named\" action=\"dlc.add\" args=\"{given}\""),
        )
    };
    // One unnamed term: spliced, and checked as a game id.
    assert!(run("{$id}").is_empty());
    assert_eq!(codes(&run("{g.playtime}")), vec![Code::TypeMismatch]);
    // Several terms, or a named one, have no single value to give `id`.
    for given in ["dlc=all", "{$id} all", "{$id} {$id}"] {
        let all = run(given);
        let hit = all
            .iter()
            .find(|f| f.code == Code::ActionArgs && f.message.contains("takes one unnamed term"));
        let hit = hit.unwrap_or_else(|| panic!("`{given}`: {all:?}"));
        assert_eq!(hit.level, Level::Route("game".into()));
    }
}

#[test]
fn a_plain_text_parameter_is_one_term_and_is_never_split() {
    let comp = forwarder("one", "action k", "{k}");
    // `gui.mode dark` is one key to `settings.set`, and it isn't on the list.
    let all = spliced(
        &comp,
        "use \"one\" action=\"settings.set\" k=\"gui.mode dark\"",
    );
    let c = codes(&all);
    assert!(c.contains(&Code::SettingKey), "{all:?}");
    // The one term takes `key` and nothing else, so `value` is missing.
    assert!(c.contains(&Code::ActionArgs), "{all:?}");
}

#[test]
fn components_that_forward_to_each_other_are_a_cycle() {
    let comps = "\ncomponent \"ping\" params=\"action args\" {\n    use \"pong\" action=\"{action}\" args=\"{args}\"\n}\ncomponent \"pong\" params=\"action args\" {\n    use \"ping\" action=\"{action}\" args=\"{args}\"\n}\n";
    let all = spliced(comps, "use \"ping\" action=\"dlc.add\" args=\"{$id} all\"");
    assert!(codes(&all).contains(&Code::ComponentCycle), "{all:?}");
    let own = "\ncomponent \"self\" params=\"action args\" {\n    use \"self\" action=\"{action}\" args=\"{args}\"\n}\n";
    let all = spliced(own, "use \"self\" action=\"dlc.add\" args=\"{$id} all\"");
    assert!(codes(&all).contains(&Code::ComponentCycle), "{all:?}");
}

#[test]
fn a_let_over_one_parameter_leaves_the_others_spliced() {
    // `k` is rebound, so `{k}` is a variable and the key is dynamic; `{args}` still
    // stands for what the `use` wrote.
    let comp = "\ncomponent \"mix\" params=\"action k args\" {\n    let k=\"{game($id).slug}\"\n    button \"B\" action=\"{action}\" args=\"{k} {args}\"\n}\n";
    let use_ =
        |args: &str| format!("use \"mix\" action=\"settings.set\" k=\"gui.mode\" args=\"{args}\"");
    let all = spliced(comp, &use_("dark"));
    assert_eq!(codes(&all), vec![Code::SettingKey], "{all:?}");
    // Without the `let` the same component is clean.
    let plain = comp.replace("    let k=\"{game($id).slug}\"\n", "");
    assert!(spliced(&plain, &use_("dark")).is_empty());
    // And the spliced `args` is still checked: one term too many.
    let all = spliced(&plain, &use_("dark extra"));
    assert_eq!(codes(&all), vec![Code::ActionArgs], "{all:?}");
}

#[test]
fn a_spliced_route_and_a_spliced_ui_variable_are_checked() {
    let go = forwarder("go", "action args", "{args}");
    let run = |args: &str| {
        spliced(
            &go,
            &format!("use \"go\" action=\"nav.go\" args=\"{args}\""),
        )
    };
    assert!(run("game {$id} files").is_empty());
    assert_eq!(codes(&run("nowhere")), vec![Code::UnknownRoute]);
    assert_eq!(
        codes(&run("game {$id} nothing")),
        vec![Code::UnknownSection]
    );
    let set = forwarder("setui", "action args", "{args}");
    let run = |args: &str| {
        spliced(
            &set,
            &format!("use \"setui\" action=\"ui.set\" args=\"{args}\""),
        )
    };
    assert!(run("view list").is_empty());
    assert_eq!(codes(&run("nothing list")), vec![Code::ActionArgs]);
    // The variable is a literal at the `use`, not a value read from data.
    let all = run("{g.title} list");
    assert!(!all.is_empty(), "{all:?}");
}

#[test]
fn a_mixed_forward_is_checked_at_two_levels() {
    // `top` adds `all` to what its `use` wrote, `mid` adds `extra`.
    let comps = "\ncomponent \"mid\" params=\"action args\" {\n    use \"summary-tile\" mark=\"check\" title=\"T\" link=\"L\" action=\"{action}\" args=\"{args} extra\"\n}\ncomponent \"top\" params=\"action args\" {\n    use \"mid\" action=\"{action}\" args=\"{args} all\"\n}\n";
    // `dlc.add` takes an id and a dlc: `{$id}`, `all`, and `extra` is one too many.
    let all = spliced(comps, "use \"top\" action=\"dlc.add\" args=\"{$id}\"");
    assert_eq!(codes(&all), vec![Code::ActionArgs], "{all:?}");
    assert!(all[0].message.contains("found more"), "{}", all[0].message);
    // Two added terms after an id leave `game.verify` with a surplus of two.
    let all = spliced(comps, "use \"top\" action=\"game.verify\" args=\"{$id}\"");
    assert_eq!(codes(&all).len(), 2, "{all:?}");
    // A type error in what the outermost `use` wrote is still found.
    let all = spliced(
        comps,
        "use \"top\" action=\"dlc.add\" args=\"{g.playtime}\"",
    );
    assert!(codes(&all).contains(&Code::TypeMismatch), "{all:?}");
}

#[test]
fn a_section_listed_twice_in_tabs_is_refused() {
    let mut files = base();
    rep(
        &mut files,
        GAME,
        "sections=\"overview builds",
        "sections=\"overview overview builds",
    );
    let all = findings(load(files));
    let dup = all
        .iter()
        .find(|f| f.code == Code::UnknownSection)
        .unwrap_or_else(|| panic!("{all:?}"));
    assert!(dup.message.contains("twice"), "{}", dup.message);
    assert_eq!(dup.level, Level::Route("game".into()));
    assert_eq!(dup.text.as_deref(), Some("overview"));
}
