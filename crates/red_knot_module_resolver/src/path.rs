use std::fmt;

use ruff_db::file_system::{FileSystemPath, FileSystemPathBuf};
use ruff_db::vfs::{system_path_to_file, VfsPath};

use crate::db::Db;
use crate::module_name::ModuleName;
use crate::supported_py_version::get_target_py_version;
use crate::typeshed::{parse_typeshed_versions, TypeshedVersions, TypeshedVersionsQueryResult};

/// Enumeration of the different kinds of search paths type checkers are expected to support.
///
/// N.B. Although we don't implement `Ord` for this enum, they are ordered in terms of the
/// priority that we want to give these modules when resolving them,
/// as per [the order given in the typing spec]
///
/// [the order given in the typing spec]: https://typing.readthedocs.io/en/latest/spec/distributing.html#import-resolution-ordering
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ModuleResolutionPathBufInner {
    Extra(FileSystemPathBuf),
    FirstParty(FileSystemPathBuf),
    StandardLibrary(FileSystemPathBuf),
    SitePackages(FileSystemPathBuf),
}

impl ModuleResolutionPathBufInner {
    fn push(&mut self, component: &str) {
        if cfg!(debug_assertions) {
            if let Some(extension) = camino::Utf8Path::new(component).extension() {
                match self {
                    Self::Extra(_) | Self::FirstParty(_) | Self::SitePackages(_) => assert!(
                        matches!(extension, "pyi" | "py"),
                        "Extension must be `py` or `pyi`; got `{extension}`"
                    ),
                    Self::StandardLibrary(_) => {
                        assert!(
                            matches!(component.matches('.').count(), 0 | 1),
                            "Component can have at most one '.'; got {component}"
                        );
                        assert_eq!(
                            extension, "pyi",
                            "Extension must be `pyi`; got `{extension}`"
                        );
                    }
                };
            }
        }
        let inner = match self {
            Self::Extra(ref mut path) => path,
            Self::FirstParty(ref mut path) => path,
            Self::StandardLibrary(ref mut path) => path,
            Self::SitePackages(ref mut path) => path,
        };
        inner.push(component);
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct ModuleResolutionPathBuf(ModuleResolutionPathBufInner);

impl ModuleResolutionPathBuf {
    /// Push a new part to the path,
    /// while maintaining the invariant that the path can only have `.py` or `.pyi` extensions.
    /// For the stdlib variant specifically, it may only have a `.pyi` extension.
    ///
    /// ## Panics:
    /// If a component with an invalid extension is passed
    pub(crate) fn push(&mut self, component: &str) {
        self.0.push(component);
    }

    #[must_use]
    pub(crate) fn extra(path: impl Into<FileSystemPathBuf>) -> Option<Self> {
        let path = path.into();
        path.extension()
            .map_or(true, |ext| matches!(ext, "py" | "pyi"))
            .then_some(Self(ModuleResolutionPathBufInner::Extra(path)))
    }

    #[must_use]
    pub(crate) fn first_party(path: impl Into<FileSystemPathBuf>) -> Option<Self> {
        let path = path.into();
        path.extension()
            .map_or(true, |ext| matches!(ext, "pyi" | "py"))
            .then_some(Self(ModuleResolutionPathBufInner::FirstParty(path)))
    }

    #[must_use]
    pub(crate) fn standard_library(path: impl Into<FileSystemPathBuf>) -> Option<Self> {
        let path = path.into();
        if path.file_stem().is_some_and(|stem| stem.contains('.')) {
            return None;
        }
        path.extension()
            .map_or(true, |ext| ext == "pyi")
            .then_some(Self(ModuleResolutionPathBufInner::StandardLibrary(path)))
    }

    #[must_use]
    pub(crate) fn stdlib_from_typeshed_root(typeshed_root: &FileSystemPath) -> Option<Self> {
        Self::standard_library(typeshed_root.join(FileSystemPath::new("stdlib")))
    }

    #[must_use]
    pub(crate) fn site_packages(path: impl Into<FileSystemPathBuf>) -> Option<Self> {
        let path = path.into();
        path.extension()
            .map_or(true, |ext| matches!(ext, "pyi" | "py"))
            .then_some(Self(ModuleResolutionPathBufInner::SitePackages(path)))
    }

    #[must_use]
    pub(crate) fn is_regular_package(&self, db: &dyn Db, search_path: &Self) -> bool {
        ModuleResolutionPathRef::from(self).is_regular_package(db, search_path)
    }

    #[must_use]
    pub(crate) fn is_directory(&self, db: &dyn Db, search_path: &Self) -> bool {
        ModuleResolutionPathRef::from(self).is_directory(db, search_path)
    }

    #[must_use]
    pub(crate) fn with_pyi_extension(&self) -> Self {
        ModuleResolutionPathRef::from(self).with_pyi_extension()
    }

    #[must_use]
    pub(crate) fn with_py_extension(&self) -> Option<Self> {
        ModuleResolutionPathRef::from(self).with_py_extension()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn join(&self, component: &str) -> Self {
        Self(ModuleResolutionPathRefInner::from(&self.0).join(component))
    }

    #[must_use]
    pub(crate) fn relativize_path<'a>(
        &'a self,
        absolute_path: &'a FileSystemPath,
    ) -> Option<ModuleResolutionPathRef<'a>> {
        ModuleResolutionPathRef::from(self).relativize_path(absolute_path)
    }
}

impl From<ModuleResolutionPathBuf> for VfsPath {
    fn from(value: ModuleResolutionPathBuf) -> Self {
        VfsPath::FileSystem(match value.0 {
            ModuleResolutionPathBufInner::Extra(path) => path,
            ModuleResolutionPathBufInner::FirstParty(path) => path,
            ModuleResolutionPathBufInner::StandardLibrary(path) => path,
            ModuleResolutionPathBufInner::SitePackages(path) => path,
        })
    }
}

impl AsRef<FileSystemPath> for ModuleResolutionPathBuf {
    #[inline]
    fn as_ref(&self) -> &FileSystemPath {
        ModuleResolutionPathRefInner::from(&self.0).as_file_system_path()
    }
}

impl fmt::Debug for ModuleResolutionPathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (name, path) = match &self.0 {
            ModuleResolutionPathBufInner::Extra(path) => ("Extra", path),
            ModuleResolutionPathBufInner::FirstParty(path) => ("FirstParty", path),
            ModuleResolutionPathBufInner::SitePackages(path) => ("SitePackages", path),
            ModuleResolutionPathBufInner::StandardLibrary(path) => ("StandardLibary", path),
        };
        f.debug_tuple(&format!("ModuleResolutionPath::{name}"))
            .field(path)
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
enum ModuleResolutionPathRefInner<'a> {
    Extra(&'a FileSystemPath),
    FirstParty(&'a FileSystemPath),
    StandardLibrary(&'a FileSystemPath),
    SitePackages(&'a FileSystemPath),
}

impl<'a> ModuleResolutionPathRefInner<'a> {
    #[must_use]
    fn load_typeshed_versions<'db>(
        db: &'db dyn Db,
        stdlib_root: &FileSystemPath,
    ) -> &'db TypeshedVersions {
        let versions_path = stdlib_root.join("VERSIONS");
        let Some(versions_file) = system_path_to_file(db.upcast(), &versions_path) else {
            todo!(
                "Still need to figure out how to handle VERSIONS files being deleted \
                from custom typeshed directories! Expected a file to exist at {versions_path}"
            )
        };
        // TODO(Alex/Micha): If VERSIONS is invalid,
        // this should invalidate not just the specific module resolution we're currently attempting,
        // but all type inference that depends on any standard-library types.
        // Unwrapping here is not correct...
        parse_typeshed_versions(db, versions_file).as_ref().unwrap()
    }

    #[must_use]
    fn is_directory(&self, db: &dyn Db, search_path: Self) -> bool {
        match (self, search_path) {
            (Self::Extra(path), Self::Extra(_)) => db.file_system().is_directory(path),
            (Self::FirstParty(path), Self::FirstParty(_)) => db.file_system().is_directory(path),
            (Self::SitePackages(path), Self::SitePackages(_)) => db.file_system().is_directory(path),
            (Self::StandardLibrary(path), Self::StandardLibrary(stdlib_root)) => {
                let Some(module_name) = ModuleResolutionPathRef(*self).to_module_name() else {
                    return false;
                };
                let typeshed_versions = Self::load_typeshed_versions(db, stdlib_root);
                match typeshed_versions.query_module(&module_name, get_target_py_version(db)) {
                    TypeshedVersionsQueryResult::Exists
                    | TypeshedVersionsQueryResult::MaybeExists => {
                        db.file_system().is_directory(path)
                    }
                    TypeshedVersionsQueryResult::DoesNotExist => false,
                }
            }
            (path, root) => unreachable!(
                "The search path should always be the same variant as `self` (got: {path:?}, {root:?})"
            )
        }
    }

    #[must_use]
    fn is_regular_package(&self, db: &dyn Db, search_path: Self) -> bool {
        match (self, search_path) {
            (Self::Extra(path), Self::Extra(_))
            | (Self::FirstParty(path), Self::FirstParty(_))
            | (Self::SitePackages(path), Self::SitePackages(_)) => {
                let file_system = db.file_system();
                file_system.exists(&path.join("__init__.py"))
                    || file_system.exists(&path.join("__init__.pyi"))
            }
            // Unlike the other variants:
            // (1) Account for VERSIONS
            // (2) Only test for `__init__.pyi`, not `__init__.py`
            (Self::StandardLibrary(path), Self::StandardLibrary(stdlib_root)) => {
                let Some(module_name) = ModuleResolutionPathRef(*self).to_module_name() else {
                    return false;
                };
                let typeshed_versions = Self::load_typeshed_versions(db, stdlib_root);
                match typeshed_versions.query_module(&module_name, get_target_py_version(db)) {
                    TypeshedVersionsQueryResult::Exists
                    | TypeshedVersionsQueryResult::MaybeExists => {
                        db.file_system().exists(&path.join("__init__.pyi"))
                    }
                    TypeshedVersionsQueryResult::DoesNotExist => false,
                }
            }
            (path, root) => unreachable!(
                "The search path should always be the same variant as `self` (got: {path:?}, {root:?})"
            )
        }
    }

    #[must_use]
    pub(crate) fn to_module_name(self) -> Option<ModuleName> {
        let (fs_path, skip_final_part) = match self {
            Self::Extra(path) | Self::FirstParty(path) | Self::SitePackages(path) => (
                path,
                path.ends_with("__init__.py") || path.ends_with("__init__.pyi"),
            ),
            Self::StandardLibrary(path) => (path, path.ends_with("__init__.pyi")),
        };

        let parent_components = fs_path
            .parent()?
            .components()
            .map(|component| component.as_str());

        if skip_final_part {
            ModuleName::from_components(parent_components)
        } else {
            ModuleName::from_components(parent_components.chain(fs_path.file_stem()))
        }
    }

    #[must_use]
    #[inline]
    fn as_file_system_path(self) -> &'a FileSystemPath {
        match self {
            Self::Extra(path) => path,
            Self::FirstParty(path) => path,
            Self::StandardLibrary(path) => path,
            Self::SitePackages(path) => path,
        }
    }

    #[must_use]
    fn with_pyi_extension(&self) -> ModuleResolutionPathBufInner {
        match self {
            Self::Extra(path) => ModuleResolutionPathBufInner::Extra(path.with_extension("pyi")),
            Self::FirstParty(path) => {
                ModuleResolutionPathBufInner::FirstParty(path.with_extension("pyi"))
            }
            Self::StandardLibrary(path) => {
                ModuleResolutionPathBufInner::StandardLibrary(path.with_extension("pyi"))
            }
            Self::SitePackages(path) => {
                ModuleResolutionPathBufInner::SitePackages(path.with_extension("pyi"))
            }
        }
    }

    #[must_use]
    fn with_py_extension(&self) -> Option<ModuleResolutionPathBufInner> {
        match self {
            Self::Extra(path) => Some(ModuleResolutionPathBufInner::Extra(
                path.with_extension("py"),
            )),
            Self::FirstParty(path) => Some(ModuleResolutionPathBufInner::FirstParty(
                path.with_extension("py"),
            )),
            Self::StandardLibrary(_) => None,
            Self::SitePackages(path) => Some(ModuleResolutionPathBufInner::SitePackages(
                path.with_extension("py"),
            )),
        }
    }

    #[cfg(test)]
    #[must_use]
    fn to_path_buf(self) -> ModuleResolutionPathBufInner {
        match self {
            Self::Extra(path) => ModuleResolutionPathBufInner::Extra(path.to_path_buf()),
            Self::FirstParty(path) => ModuleResolutionPathBufInner::FirstParty(path.to_path_buf()),
            Self::StandardLibrary(path) => {
                ModuleResolutionPathBufInner::StandardLibrary(path.to_path_buf())
            }
            Self::SitePackages(path) => {
                ModuleResolutionPathBufInner::SitePackages(path.to_path_buf())
            }
        }
    }

    #[cfg(test)]
    #[must_use]
    fn join(
        &self,
        component: &'a (impl AsRef<FileSystemPath> + ?Sized),
    ) -> ModuleResolutionPathBufInner {
        let mut result = self.to_path_buf();
        result.push(component.as_ref().as_str());
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModuleResolutionPathRef<'a>(ModuleResolutionPathRefInner<'a>);

impl<'a> ModuleResolutionPathRef<'a> {
    #[must_use]
    pub(crate) fn extra(path: &'a (impl AsRef<FileSystemPath> + ?Sized)) -> Option<Self> {
        let path = path.as_ref();
        path.extension()
            .map_or(true, |ext| matches!(ext, "pyi" | "py"))
            .then_some(Self(ModuleResolutionPathRefInner::Extra(path)))
    }

    #[must_use]
    pub(crate) fn first_party(path: &'a (impl AsRef<FileSystemPath> + ?Sized)) -> Option<Self> {
        let path = path.as_ref();
        path.extension()
            .map_or(true, |ext| matches!(ext, "pyi" | "py"))
            .then_some(Self(ModuleResolutionPathRefInner::FirstParty(path)))
    }

    #[must_use]
    pub(crate) fn standard_library(
        path: &'a (impl AsRef<FileSystemPath> + ?Sized),
    ) -> Option<Self> {
        let path = path.as_ref();
        // Unlike other variants, only `.pyi` extensions are permitted
        path.extension()
            .map_or(true, |ext| ext == "pyi")
            .then_some(Self(ModuleResolutionPathRefInner::StandardLibrary(path)))
    }

    #[must_use]
    pub(crate) fn site_packages(path: &'a (impl AsRef<FileSystemPath> + ?Sized)) -> Option<Self> {
        let path = path.as_ref();
        path.extension()
            .map_or(true, |ext| matches!(ext, "pyi" | "py"))
            .then_some(Self(ModuleResolutionPathRefInner::SitePackages(path)))
    }

    #[must_use]
    pub(crate) fn is_directory(&self, db: &dyn Db, search_path: impl Into<Self>) -> bool {
        self.0.is_directory(db, search_path.into().0)
    }

    #[must_use]
    pub(crate) fn is_regular_package(&self, db: &dyn Db, search_path: impl Into<Self>) -> bool {
        self.0.is_regular_package(db, search_path.into().0)
    }

    #[must_use]
    pub(crate) fn to_module_name(self) -> Option<ModuleName> {
        self.0.to_module_name()
    }

    #[must_use]
    pub(crate) fn with_pyi_extension(&self) -> ModuleResolutionPathBuf {
        ModuleResolutionPathBuf(self.0.with_pyi_extension())
    }

    #[must_use]
    pub(crate) fn with_py_extension(self) -> Option<ModuleResolutionPathBuf> {
        self.0.with_py_extension().map(ModuleResolutionPathBuf)
    }

    #[must_use]
    pub(crate) fn relativize_path(&self, absolute_path: &'a FileSystemPath) -> Option<Self> {
        match self.0 {
            ModuleResolutionPathRefInner::Extra(root) => {
                absolute_path.strip_prefix(root).ok().and_then(Self::extra)
            }
            ModuleResolutionPathRefInner::FirstParty(root) => absolute_path
                .strip_prefix(root)
                .ok()
                .and_then(Self::first_party),
            ModuleResolutionPathRefInner::StandardLibrary(root) => absolute_path
                .strip_prefix(root)
                .ok()
                .and_then(Self::standard_library),
            ModuleResolutionPathRefInner::SitePackages(root) => absolute_path
                .strip_prefix(root)
                .ok()
                .and_then(Self::site_packages),
        }
    }

    #[cfg(test)]
    pub(crate) fn to_path_buf(self) -> ModuleResolutionPathBuf {
        ModuleResolutionPathBuf(self.0.to_path_buf())
    }
}

impl<'a> From<&'a ModuleResolutionPathBufInner> for ModuleResolutionPathRefInner<'a> {
    #[inline]
    fn from(value: &'a ModuleResolutionPathBufInner) -> Self {
        match value {
            ModuleResolutionPathBufInner::Extra(path) => ModuleResolutionPathRefInner::Extra(path),
            ModuleResolutionPathBufInner::FirstParty(path) => {
                ModuleResolutionPathRefInner::FirstParty(path)
            }
            ModuleResolutionPathBufInner::StandardLibrary(path) => {
                ModuleResolutionPathRefInner::StandardLibrary(path)
            }
            ModuleResolutionPathBufInner::SitePackages(path) => {
                ModuleResolutionPathRefInner::SitePackages(path)
            }
        }
    }
}

impl<'a> From<&'a ModuleResolutionPathBuf> for ModuleResolutionPathRef<'a> {
    fn from(value: &'a ModuleResolutionPathBuf) -> Self {
        ModuleResolutionPathRef(ModuleResolutionPathRefInner::from(&value.0))
    }
}

impl<'a> PartialEq<ModuleResolutionPathBuf> for ModuleResolutionPathRef<'a> {
    fn eq(&self, other: &ModuleResolutionPathBuf) -> bool {
        match (self.0, &other.0) {
            (
                ModuleResolutionPathRefInner::Extra(self_path),
                ModuleResolutionPathBufInner::Extra(other_path),
            )
            | (
                ModuleResolutionPathRefInner::FirstParty(self_path),
                ModuleResolutionPathBufInner::FirstParty(other_path),
            )
            | (
                ModuleResolutionPathRefInner::StandardLibrary(self_path),
                ModuleResolutionPathBufInner::StandardLibrary(other_path),
            )
            | (
                ModuleResolutionPathRefInner::SitePackages(self_path),
                ModuleResolutionPathBufInner::SitePackages(other_path),
            ) => *self_path == **other_path,
            _ => false,
        }
    }
}

impl<'a> PartialEq<FileSystemPath> for ModuleResolutionPathRef<'a> {
    fn eq(&self, other: &FileSystemPath) -> bool {
        self.0.as_file_system_path() == other
    }
}

impl<'a> PartialEq<ModuleResolutionPathRef<'a>> for FileSystemPath {
    fn eq(&self, other: &ModuleResolutionPathRef<'a>) -> bool {
        self == other.0.as_file_system_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use insta::assert_debug_snapshot;

    #[test]
    fn constructor_rejects_non_pyi_stdlib_paths() {
        assert!(ModuleResolutionPathBuf::standard_library("foo.py").is_none());
        assert!(ModuleResolutionPathBuf::standard_library("foo/__init__.py").is_none());
        assert!(ModuleResolutionPathBuf::standard_library("foo.py.pyi").is_none());
    }

    fn stdlib_path_test_case(path: &str) -> ModuleResolutionPathBuf {
        ModuleResolutionPathBuf::standard_library(path).unwrap()
    }

    #[test]
    fn stdlib_path_no_extension() {
        assert_debug_snapshot!(stdlib_path_test_case("foo"), @r###"
        ModuleResolutionPath::StandardLibary(
            "foo",
        )
        "###);
    }

    #[test]
    fn stdlib_path_pyi_extension() {
        assert_debug_snapshot!(stdlib_path_test_case("foo.pyi"), @r###"
        ModuleResolutionPath::StandardLibary(
            "foo.pyi",
        )
        "###);
    }

    #[test]
    fn stdlib_path_dunder_init() {
        assert_debug_snapshot!(stdlib_path_test_case("foo/__init__.pyi"), @r###"
        ModuleResolutionPath::StandardLibary(
            "foo/__init__.pyi",
        )
        "###);
    }

    #[test]
    fn stdlib_paths_can_only_be_pyi() {
        assert!(stdlib_path_test_case("foo").with_py_extension().is_none());
    }

    #[test]
    fn stdlib_path_with_pyi_extension() {
        assert_debug_snapshot!(
            ModuleResolutionPathBuf::standard_library("foo").unwrap().with_pyi_extension(),
            @r###"
        ModuleResolutionPath::StandardLibary(
            "foo.pyi",
        )
        "###
        );
    }

    #[test]
    fn non_stdlib_path_with_py_extension() {
        assert_debug_snapshot!(
            ModuleResolutionPathBuf::first_party("foo").unwrap().with_py_extension().unwrap(),
            @r###"
        ModuleResolutionPath::FirstParty(
            "foo.py",
        )
        "###
        );
    }

    #[test]
    fn non_stdlib_path_with_pyi_extension() {
        assert_debug_snapshot!(
            ModuleResolutionPathBuf::first_party("foo").unwrap().with_pyi_extension(),
            @r###"
        ModuleResolutionPath::FirstParty(
            "foo.pyi",
        )
        "###
        );
    }

    fn non_stdlib_module_name_test_case(path: &str) -> ModuleName {
        let variants = [
            ModuleResolutionPathRef::extra,
            ModuleResolutionPathRef::first_party,
            ModuleResolutionPathRef::site_packages,
        ];
        let results: Vec<Option<ModuleName>> = variants
            .into_iter()
            .map(|variant| variant(path).unwrap().to_module_name())
            .collect();
        assert!(results
            .iter()
            .zip(results.iter().take(1))
            .all(|(this, next)| this == next));
        results.into_iter().next().unwrap().unwrap()
    }

    #[test]
    fn module_name_1_part_no_extension() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo"), @r###"
        ModuleName(
            "foo",
        )
        "###);
    }

    #[test]
    fn module_name_one_part_pyi() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo.pyi"), @r###"
        ModuleName(
            "foo",
        )
        "###);
    }

    #[test]
    fn module_name_one_part_py() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo.py"), @r###"
        ModuleName(
            "foo",
        )
        "###);
    }

    #[test]
    fn module_name_2_parts_dunder_init_py() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/__init__.py"), @r###"
        ModuleName(
            "foo",
        )
        "###);
    }

    #[test]
    fn module_name_2_parts_dunder_init_pyi() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/__init__.pyi"), @r###"
        ModuleName(
            "foo",
        )
        "###);
    }

    #[test]
    fn module_name_2_parts_no_extension() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/bar"), @r###"
        ModuleName(
            "foo.bar",
        )
        "###);
    }

    #[test]
    fn module_name_2_parts_pyi() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/bar.pyi"), @r###"
        ModuleName(
            "foo.bar",
        )
        "###);
    }

    #[test]
    fn module_name_2_parts_py() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/bar.py"), @r###"
        ModuleName(
            "foo.bar",
        )
        "###);
    }

    #[test]
    fn module_name_3_parts_dunder_init_pyi() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/bar/__init__.pyi"), @r###"
        ModuleName(
            "foo.bar",
        )
        "###);
    }

    #[test]
    fn module_name_3_parts_dunder_init_py() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/bar/__init__.py"), @r###"
        ModuleName(
            "foo.bar",
        )
        "###);
    }

    #[test]
    fn module_name_3_parts_no_extension() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/bar/baz"), @r###"
        ModuleName(
            "foo.bar.baz",
        )
        "###);
    }

    #[test]
    fn module_name_3_parts_pyi() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/bar/baz.pyi"), @r###"
        ModuleName(
            "foo.bar.baz",
        )
        "###);
    }

    #[test]
    fn module_name_3_parts_py() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/bar/baz.py"), @r###"
        ModuleName(
            "foo.bar.baz",
        )
        "###);
    }

    #[test]
    fn module_name_4_parts_dunder_init_pyi() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/bar/baz/__init__.pyi"), @r###"
        ModuleName(
            "foo.bar.baz",
        )
        "###);
    }

    #[test]
    fn module_name_4_parts_dunder_init_py() {
        assert_debug_snapshot!(non_stdlib_module_name_test_case("foo/bar/baz/__init__.py"), @r###"
        ModuleName(
            "foo.bar.baz",
        )
        "###);
    }

    #[test]
    fn join_1() {
        assert_debug_snapshot!(ModuleResolutionPathBuf::standard_library("foo").unwrap().join("bar"), @r###"
        ModuleResolutionPath::StandardLibary(
            "foo/bar",
        )
        "###);
    }

    #[test]
    fn join_2() {
        assert_debug_snapshot!(ModuleResolutionPathBuf::site_packages("foo").unwrap().join("bar.pyi"), @r###"
        ModuleResolutionPath::SitePackages(
            "foo/bar.pyi",
        )
        "###);
    }

    #[test]
    fn join_3() {
        assert_debug_snapshot!(ModuleResolutionPathBuf::extra("foo").unwrap().join("bar.py"), @r###"
        ModuleResolutionPath::Extra(
            "foo/bar.py",
        )
        "###);
    }

    #[test]
    fn join_4() {
        assert_debug_snapshot!(
            ModuleResolutionPathBuf::first_party("foo").unwrap().join("bar/baz/eggs/__init__.py"),
            @r###"
        ModuleResolutionPath::FirstParty(
            "foo/bar/baz/eggs/__init__.py",
        )
        "###
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Extension must be `pyi`; got `py`")]
    fn stdlib_path_invalid_join_py() {
        stdlib_path_test_case("foo").push("bar.py");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Extension must be `pyi`; got `rs`")]
    fn stdlib_path_invalid_join_rs() {
        stdlib_path_test_case("foo").push("bar.rs");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Extension must be `py` or `pyi`; got `rs`")]
    fn non_stdlib_path_invalid_join_rs() {
        ModuleResolutionPathBuf::site_packages("foo")
            .unwrap()
            .push("bar.rs");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Component can have at most one '.'")]
    fn invalid_stdlib_join_too_many_extensions() {
        stdlib_path_test_case("foo").push("bar.py.pyi");
    }
}
