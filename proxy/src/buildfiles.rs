//! Build-descriptor detection, ported from the client's `buildFiles.ts`
//! (basename match, case-sensitive, any depth).

const BUILD_FILE_NAMES: &[&str] = &[
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "BUILD",
    "BUILD.bazel",
    "MODULE.bazel",
    "WORKSPACE",
    "WORKSPACE.bazel",
    ".bazelproject",
    // Beyond the official client's list: these also change the project model.
    "gradle.properties",
    "gradle-wrapper.properties",
];

pub fn is_build_file_path(path: &str) -> bool {
    let basename = path.rsplit(['/', '\\']).next().unwrap_or(path);
    BUILD_FILE_NAMES.contains(&basename) || basename.ends_with(".bzl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects() {
        assert!(is_build_file_path("/x/pom.xml"));
        assert!(is_build_file_path("/x/y/build.gradle.kts"));
        assert!(is_build_file_path("/x/MODULE.bazel"));
        assert!(is_build_file_path("/x/tools/defs.bzl"));
        assert!(is_build_file_path("/x/gradle.properties"));
        assert!(is_build_file_path("/x/gradle/wrapper/gradle-wrapper.properties"));
        assert!(!is_build_file_path("/x/build.gradle.kts.bak"));
        assert!(!is_build_file_path("/x/build.gradlex"));
        assert!(!is_build_file_path("/x/BUILDING"));
    }
}
