//! `goggame-<gogid>.script`: what GOG Galaxy runs after an install (design 18
//! §4.9). Only the registry actions are applied; the other types are skipped
//! with a warning. The script's `{app}`-style placeholders become Windows
//! paths in the prefix. Both lanes call [`load`] and merge the values in front
//! of the compat `registry` entries, which therefore win.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use super::compat::RegistryValue;

/// `RegistryValue::kind` of a value that only asks for the key to exist.
pub(crate) const KEY_ONLY: &str = "KEY";

/// What the placeholders stand for in this prefix.
pub struct Ctx<'a> {
    /// `{app}`: the game dir, e.g. `C:\GOG\Neo Cab`.
    pub app: &'a str,
    /// `{userdocs}`: the user's Documents folder.
    pub userdocs: &'a str,
    /// `{group}`: the start-menu group, which is the game's title.
    pub group: &'a str,
}

/// The registry values of a script and what was left out.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub values: Vec<RegistryValue>,
    pub warnings: Vec<String>,
}

/// The script in `game_dir`, if it has one.
pub fn load(game_dir: &Path, gogid: &str, ctx: &Ctx) -> Option<Parsed> {
    let path = game_dir.join(format!("goggame-{gogid}.script"));
    let raw = std::fs::read_to_string(&path).ok()?;
    let mut p = parse(&raw, ctx);
    for w in &mut p.warnings {
        *w = format!("{}: {w}", path.display());
    }
    Some(p)
}

/// The base game's script, then every DLC's (`goggame-<dlcid>.script`, by
/// ascending id), with the same subset (design 19 §3.6). A DLC's script sits
/// beside its info: in `game_dir` or one level down. `None` when no script
/// exists at all.
pub fn load_all(game_dir: &Path, gogid: &str, ctx: &Ctx) -> Option<Parsed> {
    let mut found = false;
    let mut all = Parsed::default();
    let mut take = |p: Option<Parsed>| {
        if let Some(p) = p {
            found = true;
            all.values.extend(p.values);
            all.warnings.extend(p.warnings);
        }
    };
    take(load(game_dir, gogid, ctx));
    let mut dlcs: Vec<String> = super::info::gog_game_ids(game_dir)
        .into_iter()
        .filter(|id| id != gogid)
        .collect();
    dlcs.sort_by_key(|id| (id.parse::<u64>().unwrap_or(u64::MAX), id.clone()));
    let mut dirs = vec![game_dir.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(game_dir) {
        let mut subs: Vec<_> = rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        subs.sort();
        dirs.extend(subs);
    }
    for id in &dlcs {
        take(dirs.iter().find_map(|d| load(d, id, ctx)));
    }
    found.then_some(all)
}

/// The `setRegistry` actions of the script text `raw`.
pub fn parse(raw: &str, ctx: &Ctx) -> Parsed {
    let mut out = Parsed::default();
    let doc: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            out.warnings
                .push(format!("not valid JSON ({e}): not applied"));
            return out;
        }
    };
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    for action in doc
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(install) = action.get("install") else {
            continue;
        };
        let kind = install.get("action").and_then(Value::as_str).unwrap_or("?");
        if !kind.eq_ignore_ascii_case("setRegistry") {
            *skipped.entry(kind.to_string()).or_default() += 1;
            continue;
        }
        let name = action.get("name").and_then(Value::as_str).unwrap_or("?");
        let args = install.get("arguments").unwrap_or(&Value::Null);
        match registry_value(args, ctx) {
            Ok(vs) => out.values.extend(vs),
            Err(why) => out.warnings.push(format!("action {name}: {why}: skipped")),
        }
    }
    for (kind, n) in skipped {
        out.warnings.push(format!(
            "{n} {kind} action(s) not run (only setRegistry is)"
        ));
    }
    let mut seen = std::collections::HashSet::new();
    out.values
        .retain(|v| seen.insert((v.key.to_lowercase(), v.name.clone(), v.data.clone())));
    out
}

fn registry_value(args: &Value, ctx: &Ctx) -> Result<Vec<RegistryValue>, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    let root = match s("root").unwrap_or("").to_ascii_uppercase().as_str() {
        "HKEY_LOCAL_MACHINE" | "HKLM" => "HKLM",
        "HKEY_CURRENT_USER" | "HKCU" => "HKCU",
        "HKEY_CLASSES_ROOT" | "HKCR" => "HKCR",
        "HKEY_USERS" | "HKU" => "HKU",
        other => return Err(format!("root {other:?} unknown")),
    };
    let subkey = s("subkey").filter(|k| !k.is_empty()).ok_or("no subkey")?;
    let key = format!("{root}\\{}", expand(subkey, ctx)?);
    let name = s("valueName").filter(|n| !n.is_empty()).map(String::from);
    let (kind, data) = match (s("valueType"), s("valueData")) {
        (None, None) if name.is_none() => (KEY_ONLY.to_string(), String::new()),
        (ty, Some(d)) => value_data(ty.unwrap_or("string"), &expand(d, ctx)?)?,
        (_, None) => return Err("no valueData".into()),
    };
    let mut out = vec![RegistryValue {
        key: key.clone(),
        name: name.clone(),
        kind: Some(kind.clone()),
        data: data.clone(),
    }];
    // A 32-bit game reads HKLM\SOFTWARE through Wow6432Node and a 64-bit one
    // directly. The script doesn't say which view Galaxy wrote, so both get it.
    if let Some(rest) = key.strip_prefix("HKLM\\")
        && rest.len() > 9
        && rest[..9].eq_ignore_ascii_case("software\\")
        && !rest[9..].to_ascii_lowercase().starts_with("wow6432node\\")
    {
        out.push(RegistryValue {
            key: format!("HKLM\\{}Wow6432Node\\{}", &rest[..9], &rest[9..]),
            name,
            kind: Some(kind),
            data,
        });
    }
    Ok(out)
}

/// `reg add` type and data for a script `valueType`/`valueData`.
fn value_data(ty: &str, data: &str) -> Result<(String, String), String> {
    match ty.to_ascii_lowercase().as_str() {
        "string" => Ok(("REG_SZ".into(), data.into())),
        "expandstring" => Ok(("REG_EXPAND_SZ".into(), data.into())),
        "dword" => {
            // `$` is hex (`$0012434f`); bare digits are decimal.
            let n = match data.strip_prefix('$') {
                Some(h) => u32::from_str_radix(h, 16),
                None => data.parse(),
            }
            .map_err(|_| format!("dword {data:?} isn't a number"))?;
            Ok(("REG_DWORD".into(), n.to_string()))
        }
        "binary" => {
            let bytes = base64(data).ok_or_else(|| format!("binary {data:?} isn't base64"))?;
            Ok((
                "REG_BINARY".into(),
                bytes.iter().map(|b| format!("{b:02x}")).collect(),
            ))
        }
        other => Err(format!("value type {other:?} unsupported")),
    }
}

/// Placeholders. `{{` is a literal `{`; a `{…}` that isn't a placeholder (a
/// GUID) stays as it is. A known placeholder this lane can't fill (such as
/// `{supportDir}`) fails the action.
fn expand(text: &str, ctx: &Ctx) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix("{{") {
            out.push('{');
            rest = r;
            continue;
        }
        let Some(end) = rest.find('}') else { break };
        let name = &rest[1..end];
        match name.to_ascii_lowercase().as_str() {
            "app" => out.push_str(ctx.app),
            "userdocs" => out.push_str(ctx.userdocs),
            "group" => out.push_str(ctx.group),
            "supportdir" | "tmp" | "commonappdata" | "localappdata" | "userappdata" => {
                return Err(format!("placeholder {{{name}}} not available"));
            }
            _ => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.trim_end_matches('=').bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTX: Ctx = Ctx {
        app: r"C:\GOG\Game",
        userdocs: r"C:\users\steamuser\Documents",
        group: "Game Title",
    };

    fn set(root: &str, sub: &str, name: &str, ty: &str, data: &str) -> String {
        serde_json::json!({"install": {"action": "setRegistry", "arguments": {
            "root": root, "subkey": sub, "valueName": name, "valueType": ty, "valueData": data}},
            "name": "k"})
        .to_string()
    }

    fn script(actions: &[String]) -> String {
        format!(r#"{{"productId":"1","actions":[{}]}}"#, actions.join(","))
    }

    #[test]
    fn string_with_app_placeholder_in_hkcu() {
        let s = script(&[set(
            "HKEY_CURRENT_USER",
            r"SOFTWARE\A\1.0",
            "Dir",
            "string",
            "{app}\\X.pal",
        )]);
        let p = parse(&s, &CTX);
        assert_eq!(p.warnings, Vec::<String>::new());
        assert_eq!(
            p.values,
            vec![RegistryValue {
                key: r"HKCU\SOFTWARE\A\1.0".into(),
                name: Some("Dir".into()),
                kind: Some("REG_SZ".into()),
                data: r"C:\GOG\Game\X.pal".into(),
            }]
        );
    }

    #[test]
    fn hklm_software_lands_in_both_views_and_wow_keys_stay_single() {
        let s = script(&[
            set(
                "HKEY_LOCAL_MACHINE",
                r"Software\Maxis\SC4",
                "A",
                "string",
                "x",
            ),
            set(
                "HKEY_LOCAL_MACHINE",
                r"SOFTWARE\Wow6432Node\M",
                "B",
                "string",
                "y",
            ),
            set("HKEY_LOCAL_MACHINE", r"SYSTEM\Foo", "C", "string", "z"),
        ]);
        let keys: Vec<_> = parse(&s, &CTX).values.into_iter().map(|v| v.key).collect();
        assert_eq!(
            keys,
            [
                r"HKLM\Software\Maxis\SC4",
                r"HKLM\Software\Wow6432Node\Maxis\SC4",
                r"HKLM\SOFTWARE\Wow6432Node\M",
                r"HKLM\SYSTEM\Foo"
            ]
        );
    }

    #[test]
    fn value_types() {
        let s = script(&[
            set("HKEY_CURRENT_USER", "K", "h", "dword", "$0012434f"),
            set("HKEY_CURRENT_USER", "K", "d", "DWORD", "00000007"),
            set(
                "HKEY_CURRENT_USER",
                "K",
                "b",
                "binary",
                "ZABwAHcAcwBvAGMAawB4AC4AZABsAGwAAAA=",
            ),
        ]);
        let v = parse(&s, &CTX).values;
        assert_eq!(
            (v[0].kind.as_deref(), v[0].data.as_str()),
            (Some("REG_DWORD"), "1196879")
        );
        assert_eq!(v[1].data, "7");
        assert_eq!(v[2].kind.as_deref(), Some("REG_BINARY"));
        assert!(v[2].data.starts_with("6400700077007300"));
    }

    #[test]
    fn empty_name_is_the_default_value_and_no_value_makes_the_key() {
        let s = script(&[
            set("HKEY_CURRENT_USER", "K", "", "string", "v"),
            r#"{"install":{"action":"setRegistry","arguments":{"root":"HKEY_CURRENT_USER","subkey":"Only"}}}"#.into(),
        ]);
        let v = parse(&s, &CTX).values;
        assert_eq!(v[0].name, None);
        assert_eq!(v[1].kind.as_deref(), Some(KEY_ONLY));
    }

    #[test]
    fn braces_double_brace_and_guid() {
        assert_eq!(expand("{{A-B}", &CTX).unwrap(), "{A-B}");
        assert_eq!(expand("{BD83-1}x", &CTX).unwrap(), "{BD83-1}x");
        assert_eq!(expand("{group}", &CTX).unwrap(), "Game Title");
        assert!(expand("{supportDir}/x", &CTX).is_err());
    }

    #[test]
    fn other_actions_and_bad_values_warn_without_failing() {
        let s = script(&[
            r#"{"install":{"action":"Execute","arguments":{"executable":"x.exe"}}}"#.into(),
            r#"{"install":{"action":"Execute","arguments":{}}}"#.into(),
            set("HKEY_CURRENT_USER", "K", "n", "multiString", "x"),
            set("HKEY_CURRENT_USER", "K", "ok", "string", "fine"),
            set("HKEY_CURRENT_USER", "K", "p", "string", "{supportDir}"),
        ]);
        let p = parse(&s, &CTX);
        assert_eq!(p.values.len(), 1);
        assert_eq!(p.warnings.len(), 3, "{:?}", p.warnings);
        assert!(p.warnings.iter().any(|w| w.contains("2 Execute")));
        assert!(parse("nope", &CTX).warnings[0].contains("not valid JSON"));
    }

    #[test]
    fn a_key_only_value_adds_just_the_key() {
        let v = RegistryValue {
            key: r"HKCU\Only".into(),
            name: None,
            kind: Some(KEY_ONLY.into()),
            data: String::new(),
        };
        assert_eq!(
            crate::macapp::setup::reg_add_argv(&v),
            ["reg", "add", r"HKCU\Only", "/f"]
        );
    }

    #[test]
    fn load_reads_the_script_from_the_game_dir() {
        let d = tempfile::tempdir().unwrap();
        assert!(load(d.path(), "7", &CTX).is_none());
        std::fs::write(
            d.path().join("goggame-7.script"),
            script(&[set("HKEY_CURRENT_USER", "K", "n", "string", "v")]),
        )
        .unwrap();
        assert_eq!(load(d.path(), "7", &CTX).unwrap().values.len(), 1);
    }

    #[test]
    fn dlc_scripts_follow_the_base_by_id() {
        let d = tempfile::tempdir().unwrap();
        for id in ["5", "20", "3"] {
            std::fs::write(d.path().join(format!("goggame-{id}.info")), "{}").unwrap();
        }
        let put = |dir: &Path, id: &str, v: &str| {
            std::fs::write(
                dir.join(format!("goggame-{id}.script")),
                script(&[set("HKEY_CURRENT_USER", "K", id, "string", v)]),
            )
            .unwrap();
        };
        let sub = d.path().join("addon");
        std::fs::create_dir(&sub).unwrap();
        put(d.path(), "5", "base");
        put(d.path(), "20", "dlc-b");
        // beside its info, one level down
        std::fs::write(sub.join("goggame-3.info"), "{}").unwrap();
        put(&sub, "3", "dlc-a");
        let p = load_all(d.path(), "5", &CTX).unwrap();
        let datas: Vec<_> = p.values.iter().map(|v| v.data.as_str()).collect();
        assert_eq!(datas, ["base", "dlc-a", "dlc-b"]);
        assert!(load_all(sub.as_path(), "9", &CTX).is_some());
        let none = tempfile::tempdir().unwrap();
        assert!(load_all(none.path(), "5", &CTX).is_none());
    }
}
