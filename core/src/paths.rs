//! Where the game's folders (`config/`, `data/`, `gallery/`, `saves/`) are,
//! whichever way a binary was started.

use std::path::{Path, PathBuf};

/// Pins the game root explicitly, overriding the search in `enter_game_root`.
pub const GAME_ROOT_ENV: &str = "ARPG_ROOT";

/// Finds the game root and makes it the working directory, so every
/// relative path the game reads (`config/gameplay.ron`, `data/...`,
/// `saves/...`) resolves the same way whether the binary was started by
/// `cargo run`/`cargo dev` from the repo, as `target/debug/*.exe` directly,
/// or as an installed client from a shortcut. Returns the root as an
/// absolute path.
///
/// Looks at, in order: `ARPG_ROOT`; the current directory; the executable's
/// folder and every folder above it (an installed client sits next to its
/// `config/` and `data/`, a dev build two levels below the repo root). If
/// nothing matches, the working directory is left as it was, and the usual
/// "failed to read config/..." errors say what's missing.
pub fn enter_game_root() -> PathBuf {
    let explicit = std::env::var_os(GAME_ROOT_ENV).map(PathBuf::from);
    let cwd = std::env::current_dir().ok();
    let exe_dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    if let Some(root) = find_game_root(explicit, cwd, exe_dir) {
        if let Err(e) = std::env::set_current_dir(&root) {
            eprintln!("[paths] could not enter game root {}: {e}", root.display());
        }
    }
    std::env::current_dir().unwrap_or_default()
}

fn find_game_root(explicit: Option<PathBuf>, cwd: Option<PathBuf>, exe_dir: Option<PathBuf>) -> Option<PathBuf> {
    if explicit.is_some() {
        return explicit;
    }
    cwd.into_iter()
        .chain(exe_dir.iter().flat_map(|dir| dir.ancestors().map(Path::to_path_buf)))
        .find(|dir| is_game_root(dir))
}

fn is_game_root(dir: &Path) -> bool {
    dir.join("config").is_dir() && dir.join("data").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<tmp>/<unique>/game` with `config/` + `data/`, plus `target/debug/`
    /// under it (where a dev build's exe lives) and an unrelated sibling.
    fn fixture() -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "arpg-paths-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let root = base.join("game");
        for dir in ["config", "data", "target/debug"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        (base, root, elsewhere)
    }

    #[test]
    fn finds_the_root_from_the_cwd_the_exe_folder_or_above_it() {
        let (base, root, elsewhere) = fixture();
        let dev_exe_dir = root.join("target/debug");

        assert_eq!(find_game_root(None, Some(root.clone()), None), Some(root.clone()), "cwd is the root");
        assert_eq!(
            find_game_root(None, Some(elsewhere.clone()), Some(dev_exe_dir)),
            Some(root.clone()),
            "exe in target/debug, launched from anywhere"
        );
        assert_eq!(find_game_root(None, Some(elsewhere.clone()), Some(root.clone())), Some(root.clone()), "installed: exe beside config/");
        assert_eq!(find_game_root(None, Some(elsewhere.clone()), Some(elsewhere.clone())), None, "nothing looks like a game root");
        assert_eq!(find_game_root(Some(elsewhere.clone()), Some(root.clone()), None), Some(elsewhere), "ARPG_ROOT wins");

        std::fs::remove_dir_all(base).unwrap();
    }
}
