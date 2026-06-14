use crate::common::*;

// ─── `cust new` ─────────────────────────────────────────────────────

#[test]
fn new_creates_buildable_lib_crate() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("greet");

    let out = cust(tmp.path(), ["new", "greet"]);
    assert_success(&out);

    assert!(dir.join("Cust.toml").is_file());
    assert!(dir.join("src/lib.c").is_file());
    let gitignore = fs::read_to_string(dir.join(".gitignore")).unwrap();
    assert!(gitignore.contains("/target"), "{gitignore}");

    // The scaffold should build cleanly with `cust build`.
    assert_success(&cust(&dir, ["build"]));
    assert!(dir.join("target/debug/build/greet/libgreet.a").is_file());
}

#[test]
fn new_into_existing_empty_directory_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("hi");
    fs::create_dir(&dir).unwrap();

    let out = cust(tmp.path(), ["new", "hi"]);
    assert_success(&out);
    assert!(dir.join("Cust.toml").is_file());
}

#[test]
fn new_refuses_to_clobber_nonempty_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("occupied");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("README"), "hello").unwrap();

    let out = cust(tmp.path(), ["new", "occupied"]);
    assert_failure_with(&out, "already exists and is not empty");
    // We must not have written anything.
    assert!(!dir.join("Cust.toml").exists());
}

#[test]
fn new_with_dash_in_path_sanitises_c_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("my-crate");

    assert_success(&cust(tmp.path(), ["new", "my-crate"]));

    let lib = fs::read_to_string(dir.join("src/lib.c")).unwrap();
    assert!(lib.contains("my_crate_add"), "{lib}");
    assert!(!lib.contains("my-crate_add"), "{lib}");

    // And `cust build` still works on the result.
    assert_success(&cust(&dir, ["build"]));
    assert!(dir
        .join("target/debug/build/my-crate/libmy-crate.a")
        .is_file());
}

#[test]
fn new_with_explicit_name_overrides_path() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("dirname");

    assert_success(&cust(
        tmp.path(),
        ["new", "dirname", "--name", "actual_name"],
    ));

    let toml = fs::read_to_string(dir.join("Cust.toml")).unwrap();
    assert!(toml.contains("name    = \"actual_name\""), "{toml}");
}

#[test]
fn new_rejects_invalid_name() {
    let tmp = tempfile::tempdir().unwrap();
    let out = cust(tmp.path(), ["new", "weird", "--name", "has spaces"]);
    assert_failure_with(&out, "invalid package name");
    // The directory should not have been left half-populated.
    assert!(!tmp.path().join("weird/Cust.toml").exists());
}
