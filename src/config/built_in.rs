use super::PermConfig;
use super::PermissionName;
use std::collections::BTreeMap;

pub(crate) fn get_built_ins() -> BTreeMap<PermissionName, PermConfig> {
    let mut result = BTreeMap::new();
    result.insert(
        PermissionName::from("fs"),
        perm(
            &[
                "std::fs",
                "std::path",
                "std::io",
                "std::os::linux::fs",
                "std::os::unix::fs",
                "std::os::unix::io",
                "std::os::wasi::fs",
                "std::os::wasi::io",
                "std::os::windows::fs",
                "std::os::windows::io",
                // std::env provides quite a few functions that return paths, which can in turn
                // allow filesystem access.
                "std::env",
            ],
            &[
                "std::io::stdio",
                "std::env::var",
                "std::env::var_os",
                "std::env::vars",
                "std::env::vars_os",
                "std::env::args",
            ],
        ),
    );
    result.insert(PermissionName::from("env"), perm(&["std::env"], &[]));
    result.insert(
        PermissionName::from("net"),
        perm(
            &[
                "std::net",
                "std::os::unix::net",
                "std::os::wasi::net",
                "std::os::windows::net",
            ],
            &[],
        ),
    );
    result.insert(
        PermissionName::from("process"),
        perm(
            &[
                "std::process",
                "std::unix::process",
                "std::windows::process",
            ],
            &["std::process::abort", "std::process::exit"],
        ),
    );
    result.insert(
        PermissionName::from("terminate"),
        perm(&["std::process::abort", "std::process::exit"], &[]),
    );
    result
}

fn perm(include: &[&str], exclude: &[&str]) -> PermConfig {
    PermConfig {
        include: include.iter().map(|s| s.to_string()).collect(),
        exclude: exclude.iter().map(|s| s.to_string()).collect(),
    }
}
