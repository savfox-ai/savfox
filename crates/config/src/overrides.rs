use toml::Value as TomlValue;

#[must_use] 
pub fn default_empty_table() -> TomlValue {
    TomlValue::Table(Default::default())
}

#[must_use] 
pub fn build_cli_overrides_layer(cli_overrides: &[(String, TomlValue)]) -> TomlValue {
    let mut root = default_empty_table();
    for (path, value) in cli_overrides {
        apply_toml_override(&mut root, path, value.clone());
    }
    root
}

/// Apply a single dotted-path override onto a TOML value.
fn apply_toml_override(root: &mut TomlValue, path: &str, value: TomlValue) {
    use toml::value::Table;

    let mut current = root;
    let mut segments_iter = path.split('.').peekable();

    while let Some(segment) = segments_iter.next() {
        let is_last = segments_iter.peek().is_none();

        if is_last {
            if let TomlValue::Table(table) = current {
                table.insert(segment.to_owned(), value);
            } else {
                let mut table = Table::new();
                table.insert(segment.to_owned(), value);
                *current = TomlValue::Table(table);
            }
            return;
        }

        if let TomlValue::Table(table) = current {
            current = table
                .entry(segment.to_owned())
                .or_insert_with(|| TomlValue::Table(Table::new()));
        } else {
            *current = TomlValue::Table(Table::new());
            if let TomlValue::Table(tbl) = current {
                current = tbl
                    .entry(segment.to_owned())
                    .or_insert_with(|| TomlValue::Table(Table::new()));
            }
        }
    }
}
