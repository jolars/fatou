//! Read package releases from installed Julia registries without invoking Julia.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use flate2::read::GzDecoder;

use crate::environment::Uuid;
use crate::julia_version::Version;

pub(crate) type Releases = BTreeMap<Uuid, (String, BTreeSet<Version>)>;

#[derive(Default)]
pub(crate) struct RegistryCache {
    archives: BTreeMap<PathBuf, CachedArchive>,
}

struct CachedArchive {
    stamp: (String, u64, Option<SystemTime>),
    releases: Releases,
}

impl RegistryCache {
    /// Directory registries read only requested packages. Compressed registries
    /// are scanned once per archive revision, since tar has no random access.
    pub(crate) fn releases(
        &mut self,
        depots: &[PathBuf],
        requested: &BTreeMap<Uuid, String>,
    ) -> Releases {
        let mut releases = Releases::new();
        if requested.is_empty() {
            return releases;
        }
        let mut live_archives = Vec::new();
        for depot in depots {
            let Ok(entries) = std::fs::read_dir(depot.join("registries")) else {
                continue;
            };
            let mut paths: Vec<_> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect();
            paths.sort();
            for path in &paths {
                let found = if path.is_dir() {
                    // Pkg prefers a compressed registry over a directory of the
                    // same name, including when the descriptor is corrupt.
                    let mut descriptor = path.as_os_str().to_os_string();
                    descriptor.push(".toml");
                    if paths.contains(&PathBuf::from(descriptor)) {
                        continue;
                    }
                    directory_releases(path, requested)
                } else if path
                    .extension()
                    .is_some_and(|extension| extension == "toml")
                {
                    let Some((archive, tree)) = archive_descriptor(path) else {
                        continue;
                    };
                    let Ok(meta) = archive.metadata() else {
                        continue;
                    };
                    let stamp = (tree, meta.len(), meta.modified().ok());
                    live_archives.push(archive.clone());
                    if self
                        .archives
                        .get(&archive)
                        .is_none_or(|cached| cached.stamp != stamp)
                    {
                        self.archives.insert(
                            archive.clone(),
                            CachedArchive {
                                stamp,
                                releases: archive_releases(&archive).unwrap_or_default(),
                            },
                        );
                    }
                    self.archives[&archive]
                        .releases
                        .iter()
                        .filter(|(uuid, (name, _))| requested.get(uuid) == Some(name))
                        .map(|(uuid, release)| (*uuid, release.clone()))
                        .collect()
                } else {
                    continue;
                };
                for (uuid, (name, mut versions)) in found {
                    releases
                        .entry(uuid)
                        .or_insert_with(|| (name, BTreeSet::new()))
                        .1
                        .append(&mut versions);
                }
            }
        }
        self.archives.retain(|path, _| live_archives.contains(path));
        releases
    }
}

fn archive_descriptor(path: &Path) -> Option<(PathBuf, String)> {
    let table = read_table(path)?;
    let archive = relative_path(table.get("path")?.as_str()?)?;
    let tree = table.get("git-tree-sha1")?.as_str()?.to_owned();
    Some((path.parent()?.join(archive), tree))
}

fn relative_path(raw: &str) -> Option<PathBuf> {
    let path = Path::new(raw);
    let mut relative = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => relative.push(name),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!relative.as_os_str().is_empty()).then_some(relative)
}

fn read_text(reader: impl Read) -> io::Result<String> {
    // A corrupt registry entry should not require an unbounded allocation.
    const LIMIT: u64 = 32 * 1024 * 1024;
    let mut text = String::new();
    reader.take(LIMIT + 1).read_to_string(&mut text)?;
    if text.len() as u64 > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "registry entry too large",
        ));
    }
    Ok(text)
}

fn read_table(path: &Path) -> Option<toml::Table> {
    toml::from_str(&read_text(File::open(path).ok()?).ok()?).ok()
}

fn registry_packages(table: &toml::Table) -> impl Iterator<Item = (Uuid, String, PathBuf)> + '_ {
    table
        .get("packages")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
        .filter_map(|(uuid, value)| {
            Some((
                uuid.parse().ok()?,
                value.get("name")?.as_str()?.to_owned(),
                relative_path(value.get("path")?.as_str()?)?,
            ))
        })
}

fn directory_releases(root: &Path, requested: &BTreeMap<Uuid, String>) -> Releases {
    let Some(table) = read_table(&root.join("Registry.toml")) else {
        return Releases::new();
    };
    registry_packages(&table)
        .filter_map(|(uuid, name, path)| {
            if requested.get(&uuid) != Some(&name) {
                return None;
            }
            let text = read_text(File::open(root.join(path).join("Versions.toml")).ok()?).ok()?;
            Some((uuid, (name, release_versions(&text))))
        })
        .collect()
}

fn archive_releases(path: &Path) -> io::Result<Releases> {
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(path)?));
    let mut packages = None;
    let mut versions = BTreeMap::new();
    for entry in archive.entries()? {
        let entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let Some(path) = entry.path()?.to_str().and_then(relative_path) else {
            continue;
        };
        if path == Path::new("Registry.toml") {
            packages = toml::from_str::<toml::Table>(&read_text(entry)?).ok();
        } else if path.file_name().is_some_and(|name| name == "Versions.toml") {
            let Some(parent) = path.parent() else {
                continue;
            };
            versions.insert(parent.to_path_buf(), release_versions(&read_text(entry)?));
        }
    }
    Ok(packages
        .into_iter()
        .flat_map(|table| {
            registry_packages(&table)
                .filter_map(|(uuid, name, path)| Some((uuid, (name, versions.get(&path)?.clone()))))
                .collect::<Vec<_>>()
        })
        .collect())
}

fn release_versions(text: &str) -> BTreeSet<Version> {
    toml::from_str::<toml::Table>(text)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(raw, value)| {
            let table = value.as_table()?;
            let tree = table.get("git-tree-sha1")?.as_str()?;
            if tree.len() != 40 || !tree.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            if let Some(yanked) = table.get("yanked")
                && yanked.as_bool() != Some(false)
            {
                return None;
            }
            // Build revisions do not change a compat bound. Prereleases do, and
            // must never become the default upgrade suggestion.
            let base = raw.split_once('+').map_or(raw.as_str(), |(base, _)| base);
            let parts: Vec<_> = base.split('.').collect();
            if parts.len() != 3
                || parts
                    .iter()
                    .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
            {
                return None;
            }
            base.parse().ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const UUID: &str = "12345678-1234-1234-1234-123456789abc";

    fn registry() -> String {
        format!("[packages]\n\"{UUID}\" = {{ name = \"Example\", path = \"E/Example\" }}\n")
    }

    fn write_directory(depot: &Path, version: &str) {
        let root = depot.join("registries/General");
        std::fs::create_dir_all(root.join("E/Example")).unwrap();
        std::fs::write(root.join("Registry.toml"), registry()).unwrap();
        std::fs::write(
            root.join("E/Example/Versions.toml"),
            format!("[\"{version}\"]\ngit-tree-sha1 = \"{}\"\n", "1".repeat(40)),
        )
        .unwrap();
    }

    fn write_archive(depot: &Path, version: &str) {
        let root = depot.join("registries");
        std::fs::create_dir_all(&root).unwrap();
        let file = File::create(root.join("General.tar.gz")).unwrap();
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::default(),
        ));
        // Registry.toml can follow the versions it maps to UUIDs.
        for (path, text) in [
            (
                "./E/Example/Versions.toml",
                format!("[\"{version}\"]\ngit-tree-sha1 = \"{}\"\n", "1".repeat(40)),
            ),
            ("./Registry.toml", registry()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(text.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, path, text.as_bytes())
                .unwrap();
        }
        archive
            .into_inner()
            .unwrap()
            .finish()
            .unwrap()
            .flush()
            .unwrap();
        std::fs::write(
            root.join("General.toml"),
            format!("path = \"General.tar.gz\"\ngit-tree-sha1 = \"{version}\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn directory_and_compressed_registries_refresh_and_match_uuid_and_name() {
        for compressed in [false, true] {
            let depot = tempfile::tempdir().unwrap();
            let depots = vec![depot.path().to_path_buf()];
            let requested = BTreeMap::from([(UUID.parse().unwrap(), "Example".into())]);
            let mut cache = RegistryCache::default();
            assert!(cache.releases(&depots, &requested).is_empty());
            let write = if compressed {
                write_archive
            } else {
                write_directory
            };
            for version in ["1.9.0", "1.10.0"] {
                write(depot.path(), version);
                let expected = BTreeMap::from([(
                    UUID.parse().unwrap(),
                    ("Example".into(), BTreeSet::from([version.parse().unwrap()])),
                )]);
                assert_eq!(cache.releases(&depots, &requested), expected);
                assert_eq!(cache.releases(&depots, &requested), expected);
            }
            let wrong_name = BTreeMap::from([(UUID.parse().unwrap(), "Different".into())]);
            assert!(cache.releases(&depots, &wrong_name).is_empty());
            let wrong_uuid = BTreeMap::from([(
                "00000000-0000-0000-0000-000000000000".parse().unwrap(),
                "Example".into(),
            )]);
            assert!(cache.releases(&depots, &wrong_uuid).is_empty());
            std::fs::remove_dir_all(depot.path().join("registries")).unwrap();
            assert!(cache.releases(&depots, &requested).is_empty());
        }
    }

    #[test]
    fn corrupt_archive_does_not_hide_other_registries() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        write_directory(first.path(), "9.0.0");
        write_archive(first.path(), "2.0.0");
        for (old, new) in [
            ("General", "General.Private"),
            ("General.toml", "General.Private.toml"),
        ] {
            std::fs::rename(
                first.path().join("registries").join(old),
                first.path().join("registries").join(new),
            )
            .unwrap();
        }
        write_directory(second.path(), "1.0.0");
        let depots = vec![first.path().to_path_buf(), second.path().to_path_buf()];
        let requested = BTreeMap::from([(UUID.parse().unwrap(), "Example".into())]);
        let mut cache = RegistryCache::default();
        assert_eq!(
            cache.releases(&depots, &requested)[&UUID.parse().unwrap()].1,
            BTreeSet::from([Version::new(1, 0, 0), Version::new(2, 0, 0)])
        );
        std::fs::write(first.path().join("registries/General.tar.gz"), "truncated").unwrap();
        assert_eq!(
            cache.releases(&depots, &requested)[&UUID.parse().unwrap()]
                .1
                .last()
                .copied()
                .unwrap(),
            Version::new(1, 0, 0)
        );
    }

    #[test]
    fn registry_paths_cannot_escape_the_registry() {
        for path in [
            "../Versions.toml",
            "/tmp/Versions.toml",
            "E/../../Versions.toml",
            "",
        ] {
            assert!(relative_path(path).is_none(), "{path}");
        }
        assert!(relative_path("E/Example").is_some());
    }

    #[test]
    fn latest_release_orders_numerically_and_skips_yanked_and_prereleases() {
        let text = r#"
["1.9.0"]
git-tree-sha1 = "1111111111111111111111111111111111111111"
["1.10.2+1"]
git-tree-sha1 = "1111111111111111111111111111111111111111"
["2.0.0"]
git-tree-sha1 = "1111111111111111111111111111111111111111"
yanked = true
["3.0.0-rc1"]
git-tree-sha1 = "1111111111111111111111111111111111111111"
["4.0.0.0"]
git-tree-sha1 = "1111111111111111111111111111111111111111"
"#;
        assert_eq!(
            release_versions(text),
            BTreeSet::from([Version::new(1, 9, 0), Version::new(1, 10, 2)])
        );
        assert_eq!(release_versions("broken = ["), BTreeSet::new());
        assert_eq!(
            release_versions(
                "[\"1.0.0\"]\ngit-tree-sha1 = \"1111111111111111111111111111111111111111\"\nyanked = \"false\""
            ),
            BTreeSet::new()
        );
    }
}
