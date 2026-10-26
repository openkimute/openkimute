//! Where the native game finds the converted assets: `--assets DIR`, else `OPENKUMITE_ASSETS` (either taken as given, with
//! a leading `~` for the home folder), else the first of [`places`] holding a conversion (manifest.json, which kumite_converter writes last).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The folder of the assets, from the command line, the environment (`env`), the executable's path and the OS
/// (`std::env::consts::OS`); none found: the places looked in, and how to convert.
pub fn find(args: &[String], env: &dyn Fn(&str) -> Option<OsString>, exe: Option<&Path>, os: &str) -> anyhow::Result<PathBuf> {
    if let Some(i) = args.iter().position(|a| a == "--assets") {
        let dir = args.get(i + 1).ok_or_else(|| anyhow::anyhow!("--assets DIR: the folder of the converted assets"))?;
        return given("--assets", dir.as_ref(), env);
    }
    if let Some(dir) = env("OPENKUMITE_ASSETS").filter(|v| !v.is_empty()) {
        return given("OPENKUMITE_ASSETS", &dir, env);
    }
    let places = places(env, exe, os);
    if let Some(dir) = places.iter().find(|d| d.join("manifest.json").is_file()) {
        return Ok(dir.clone());
    }
    let list: String = places.iter().map(|d| format!("\n  {}", d.display())).collect();
    anyhow::bail!(
        "no converted game found; looked in:{list}\nConvert your game disc with kumite_converter \
         (its README says how) into one of these folders, or give the folder it wrote with \
         --assets DIR or OPENKUMITE_ASSETS=DIR"
    )
}

/// A conversion copied into `dir` (Android's import of the folder or the .zip the player chose): `dir` itself when it
/// holds manifest.json, else its folder that does (a .zip of the folder); checked as the game reads it (its format:
/// [`kumite_assets::Assets::new`]).
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn conversion_in(dir: &Path) -> anyhow::Result<PathBuf> {
    let mut inner: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    inner.sort();
    let found = std::iter::once(dir.to_path_buf()).chain(inner).find(|d| d.join("manifest.json").is_file());
    let found = found.ok_or_else(|| anyhow::anyhow!("That folder has no manifest.json, so it isn’t a converted game. Choose the folder kumite_converter wrote."))?;
    kumite_assets::Assets::new(&found)?;
    Ok(found)
}

/// A folder given by hand (`what`): without the spaces around it, `~` the home folder; one that is not there is an
/// error (one without a conversion is [`kumite_assets::Assets::new`]'s).
fn given(what: &str, dir: &std::ffi::OsStr, env: &dyn Fn(&str) -> Option<OsString>) -> anyhow::Result<PathBuf> {
    let text = dir.to_string_lossy();
    let t = text.trim();
    let path = match t.strip_prefix('~').filter(|r| r.is_empty() || r.starts_with(['/', '\\'])) {
        Some(rest) => home(env).ok_or_else(|| anyhow::anyhow!("{what} {t}: no home folder for ~"))?.join(rest.trim_start_matches(['/', '\\'])),
        None if t.len() == text.len() => PathBuf::from(dir),
        None => PathBuf::from(t),
    };
    anyhow::ensure!(path.is_dir(), "{what}: there is no folder {}", path.display());
    Ok(path)
}

/// The home folder: `HOME`, else `USERPROFILE` (Windows).
fn home(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    ["HOME", "USERPROFILE"].into_iter().find_map(|k| env(k).filter(|v| !v.is_empty())).map(PathBuf::from)
}

/// In order: `assets/` next to the executable (a portable install); macOS: the .app bundle's
/// `Contents/Resources/assets`, then `~/Library/Application Support/OpenKumite/assets`; Windows:
/// `%APPDATA%\OpenKumite\assets`; elsewhere `$XDG_DATA_HOME/openkumite/assets` (`~/.local/share`), then each of
/// `$XDG_DATA_DIRS` (`/usr/local/share:/usr/share`); last the repository's own `assets/` (development).
fn places(env: &dyn Fn(&str) -> Option<OsString>, exe: Option<&Path>, os: &str) -> Vec<PathBuf> {
    let var = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = home(env);
    let dir = exe.and_then(Path::parent);
    let mut out: Vec<PathBuf> = dir.map(|d| d.join("assets")).into_iter().collect();
    match os {
        "macos" => {
            // (a bundle's executable is <bundle>/Contents/MacOS/<name>)
            out.extend(dir.filter(|d| d.ends_with("Contents/MacOS")).and_then(Path::parent).map(|c| c.join("Resources/assets")));
            out.extend(home.map(|h| h.join("Library/Application Support/OpenKumite/assets")));
        }
        "windows" => out.extend(var("APPDATA").map(|a| a.join("OpenKumite").join("assets"))),
        _ => {
            out.extend(var("XDG_DATA_HOME").filter(|d| d.is_absolute()).or_else(|| home.map(|h| h.join(".local/share"))).map(|d| d.join("openkumite/assets")));
            let dirs = env("XDG_DATA_DIRS").filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            out.extend(std::env::split_paths(&dirs).filter(|d| d.is_absolute()).map(|d| d.join("openkumite/assets")));
        }
    }
    out.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets"));
    out
}

// (its paths are unix ones)
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_first_place_with_a_conversion() {
        let root = std::env::temp_dir().join(format!("openkumite_assetdir_test_{}", std::process::id()));
        let home = root.join("home");
        let exe = root.join("Game.app/Contents/MacOS/openkumite");
        let env = |k: &str| (k == "HOME").then(|| home.clone().into_os_string());
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets");
        let bundle = root.join("Game.app/Contents/Resources/assets");
        let user = home.join("Library/Application Support/OpenKumite/assets");
        let next = root.join("Game.app/Contents/MacOS/assets");
        assert_eq!(places(&env, Some(&exe), "macos"), [next.clone(), bundle.clone(), user.clone(), repo.clone()]);
        // XDG's folders, and their defaults
        let xdg = |k: &str| match k {
            "XDG_DATA_HOME" => Some("/d".into()),
            "XDG_DATA_DIRS" => Some("/a:rel:/b".into()),
            _ => None,
        };
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(places(&xdg, None, "linux"), [p("/d/openkumite/assets"), p("/a/openkumite/assets"), p("/b/openkumite/assets"), repo.clone()]);
        let at = |k: &str| (k == "HOME").then(|| OsString::from("/h"));
        let unix = places(&at, None, "freebsd");
        assert_eq!(unix[..3], [p("/h/.local/share/openkumite/assets"), p("/usr/local/share/openkumite/assets"), p("/usr/share/openkumite/assets")]);
        let win = |k: &str| (k == "APPDATA").then(|| OsString::from("/ad"));
        assert_eq!(places(&win, None, "windows"), [p("/ad/OpenKumite/assets"), repo.clone()]);

        // the first holding manifest.json; --assets, then OPENKUMITE_ASSETS, taken as given
        let none: [String; 1] = ["openkumite".into()];
        for d in [&user, &bundle] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("manifest.json"), "{}").unwrap();
        }
        assert_eq!(find(&none, &env, Some(&exe), "macos").unwrap(), bundle);
        std::fs::remove_file(bundle.join("manifest.json")).unwrap();
        assert_eq!(find(&none, &env, Some(&exe), "macos").unwrap(), user);
        let with_env = |k: &str| if k == "OPENKUMITE_ASSETS" { Some(bundle.clone().into_os_string()) } else { env(k) };
        assert_eq!(find(&none, &with_env, Some(&exe), "macos").unwrap(), bundle);
        let args = ["openkumite".to_string(), "--assets".into(), " ~/Library/Application Support/OpenKumite/assets ".into()];
        assert_eq!(find(&args, &with_env, Some(&exe), "macos").unwrap(), user, "spaces trimmed, ~ the home folder");
        assert!(find(&args[..2], &env, Some(&exe), "macos").is_err());
        let gone = ["openkumite".to_string(), "--assets".into(), root.join("gone").display().to_string()];
        let e = find(&gone, &env, Some(&exe), "macos").map_or_else(|e| e.to_string(), |d| d.display().to_string());
        assert!(e.contains("there is no folder"), "{e}");
        // none: every place in the error (unless the repository has its own)
        let nowhere = |k: &str| (k == "HOME").then(|| root.join("nobody").into_os_string());
        let e = find(&none, &nowhere, Some(&root.join("bin/openkumite")), "macos").map_or_else(|e| e.to_string(), |d| d.display().to_string());
        if !repo.join("manifest.json").is_file() {
            assert!(e.contains(&root.join("bin/assets").display().to_string()) && e.contains("kumite_converter"), "{e}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_copied_conversion_found_and_checked() {
        let dir = std::env::temp_dir().join(format!("openkumite_conversion_in_test_{}", std::process::id()));
        let manifest = |d: &Path, format: u64| {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("manifest.json"), format!("{{\"format\": {format}}}")).unwrap();
        };
        // nothing converted: the player's words
        std::fs::create_dir_all(dir.join("photos")).unwrap();
        assert!(conversion_in(&dir).unwrap_err().to_string().contains("no manifest.json"));
        // a .zip of the folder: the one inside; an older conversion refused
        manifest(&dir.join("assets"), kumite_assets::FORMAT);
        assert_eq!(conversion_in(&dir).unwrap(), dir.join("assets"));
        manifest(&dir.join("assets"), kumite_assets::FORMAT - 1);
        assert!(conversion_in(&dir).unwrap_err().to_string().contains("older conversion"));
        // the folder itself first
        manifest(&dir, kumite_assets::FORMAT);
        assert_eq!(conversion_in(&dir).unwrap(), dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
