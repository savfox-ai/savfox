use std::path::Path;

use anyhow::{Context, Result};
use savfox_app_server_protocol::{read_schema_fixture_tree, write_schema_fixtures};
use similar::TextDiff;

#[test]
fn schema_fixtures_are_stable_across_generations() -> Result<()> {
    let first_dir = tempfile::tempdir().context("create first temp dir")?;
    let second_dir = tempfile::tempdir().context("create second temp dir")?;

    write_schema_fixtures(first_dir.path(), None).context("generate first schema fixture set")?;
    write_schema_fixtures(second_dir.path(), None).context("generate second schema fixture set")?;

    let first_tree = read_tree(first_dir.path())?;
    let second_tree = read_tree(second_dir.path())?;

    let first_paths = first_tree
        .keys()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>();
    let second_paths = second_tree
        .keys()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>();

    if first_paths != second_paths {
        let expected = first_paths.join("\n");
        let actual = second_paths.join("\n");
        let diff = TextDiff::from_lines(&expected, &actual)
            .unified_diff()
            .header("first", "second")
            .to_string();

        panic!("App-server schema file sets differ across two fresh generations.\n\n{diff}");
    }

    anyhow::ensure!(
        !first_tree.is_empty(),
        "generated schema tree is unexpectedly empty"
    );

    for (path, expected) in &first_tree {
        let actual = second_tree
            .get(path)
            .ok_or_else(|| anyhow::anyhow!("missing second generated file: {}", path.display()))?;

        if expected == actual {
            continue;
        }

        let expected_str = String::from_utf8_lossy(expected);
        let actual_str = String::from_utf8_lossy(actual);
        let diff = TextDiff::from_lines(&expected_str, &actual_str)
            .unified_diff()
            .header("first", "second")
            .to_string();
        panic!(
            "App-server schema fixture {} differs across two fresh generations.\n\n{diff}",
            path.display()
        );
    }

    Ok(())
}

fn read_tree(root: &Path) -> Result<std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>> {
    read_schema_fixture_tree(root).context("read schema fixture tree")
}
