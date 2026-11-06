//! Finds plugin sources across the category folders under `packages/`.

use std::path::{Path, PathBuf};

/// Category folders whose plugins are released as provider packages.
const RELEASED_CATEGORIES: [&str; 2] = ["providers", "middleware"];
/// Every category folder that holds Rust sources.
const SOURCE_CATEGORIES: [&str; 3] = ["providers", "middleware", "crates"];

fn packages_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The source folder of a released plugin, if any category holds it.
pub(crate) fn find_plugin_directory(id: &str) -> Option<PathBuf> {
    RELEASED_CATEGORIES
        .iter()
        .map(|category| packages_root().join(category).join(id))
        .find(|directory| directory.join("nora-plugin.json").is_file())
}

pub(crate) fn plugin_directory(id: &str) -> PathBuf {
    find_plugin_directory(id).unwrap_or_else(|| panic!("no plugin source for {id}"))
}

pub(crate) fn plugin_manifest(id: &str) -> PathBuf {
    plugin_directory(id).join("nora-plugin.json")
}

fn directories(categories: &[&str]) -> Vec<PathBuf> {
    let mut directories = categories
        .iter()
        .flat_map(|category| std::fs::read_dir(packages_root().join(category)).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    directories.sort();
    directories
}

/// Every released plugin's source folder.
pub(crate) fn released_plugin_directories() -> Vec<PathBuf> {
    directories(&RELEASED_CATEGORIES)
        .into_iter()
        .filter(|directory| directory.join("nora-plugin.json").is_file())
        .collect()
}

/// Every package folder with Rust sources, plugins and shared crates alike.
pub(crate) fn source_directories() -> Vec<PathBuf> {
    directories(&SOURCE_CATEGORIES)
}

/// One folder linking every released plugin, as the build stages them for preparation.
pub(crate) fn staged_plugin_sources(directory: &Path) -> PathBuf {
    let staged = directory.join("sources");
    std::fs::create_dir_all(&staged).unwrap();
    for source in released_plugin_directories() {
        let source = source.canonicalize().unwrap();
        std::os::unix::fs::symlink(&source, staged.join(source.file_name().unwrap())).unwrap();
    }
    staged
}
