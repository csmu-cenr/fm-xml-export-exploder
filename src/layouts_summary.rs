//! Summaries are generated from this run's layout files after all inputs finish,
//! so split catalogs are combined and stale files never enter the index.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::config::{Flags, OutputTree};
use crate::utils::attributes::{get_attribute, unescape_value};
use crate::utils::output_files::OutputFiles;
use crate::utils::output_path::build_tree_path;

pub fn write_summaries(
    root: &Path,
    db_name: &str,
    flags: &Flags,
    output: &OutputFiles,
) -> Result<()> {
    let Some(extension) = &flags.layouts_summary else {
        return Ok(());
    };
    let db_name = crate::utils::directory_name_with_replaced_spaces(db_name, flags);
    let mut summaries: BTreeMap<PathBuf, Vec<Vec<String>>> = BTreeMap::new();
    for path in output.paths() {
        if path.extension().is_none_or(|ext| ext != "xml") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let parts: Vec<_> = relative.components().collect();
        if parts.len() < 3 {
            continue;
        }
        let (db_index, domain_index) = match flags.output_tree {
            OutputTree::Db => (0, 1),
            OutputTree::Domain => (1, 0),
        };
        let domain = parts[domain_index].as_os_str().to_string_lossy();
        if domain != "layouts" && !domain.starts_with("layouts__") {
            continue;
        }
        if flags.layouts_summary_file_name.is_none()
            && parts[db_index].as_os_str() != db_name.as_str()
        {
            continue;
        }
        let layout_dir = build_tree_path(
            root,
            &flags.output_tree,
            &domain,
            &parts[db_index].as_os_str().to_string_lossy(),
        );
        let summary_path = flags
            .layouts_summary_file_name
            .clone()
            .unwrap_or_else(|| layout_dir.join(format!(".summary.{extension}")));
        let rows = summaries.entry(summary_path).or_default();
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Cannot read layout {}", path.display()))?;
        let Some((name, id)) = layout_identity(&content) else {
            continue;
        };
        let parent = path.parent().unwrap();
        let row_path = if flags.layouts_summary_file_name.is_some() {
            std::path::absolute(parent)?.to_string_lossy().into_owned()
        } else {
            parent
                .strip_prefix(&layout_dir)?
                .to_string_lossy()
                .into_owned()
        };
        rows.push(vec![
            name,
            id,
            row_path,
            path.file_name().unwrap().to_string_lossy().into_owned(),
        ]);
    }
    // An explicitly requested destination is still created when there are no layouts.
    if let Some(path) = &flags.layouts_summary_file_name {
        summaries.entry(path.clone()).or_default();
    }
    for (path, mut rows) in summaries {
        ensure!(
            !output.contains(&path),
            "Layout summary path collides with exported file: {}",
            path.display()
        );
        rows.sort_by(|a, b| (&a[2], &a[3]).cmp(&(&b[2], &b[3])));
        rows.dedup();
        let delimiter = if extension == "csv" { ',' } else { '\t' };
        let mut text = ["Name", "ID", "Path", "File"].join(&delimiter.to_string());
        text.push('\n');
        for row in rows {
            text.push_str(
                &row.iter()
                    .map(|value| quote_field(value, delimiter))
                    .collect::<Vec<_>>()
                    .join(&delimiter.to_string()),
            );
            text.push('\n');
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .with_context(|| format!("Cannot create summary directory {}", parent.display()))?;
        }
        if !fs::read(&path).is_ok_and(|existing| existing == text.as_bytes()) {
            fs::write(&path, &text)
                .with_context(|| format!("Cannot write layout summary {}", path.display()))?;
        }
        output.write(&path, &text);
    }
    Ok(())
}

fn layout_identity(xml: &str) -> Option<(String, String)> {
    let mut reader = Reader::from_str(xml);
    let mut depth = 0;
    let mut root_identity = None;
    loop {
        match reader.read_event().ok()? {
            Event::Start(tag) | Event::Empty(tag) if depth == 0 => {
                if tag.name().as_ref() != "Layout"
                    || get_attribute(&tag, "isFolder").is_some()
                    || get_attribute(&tag, "isSeparatorItem").as_deref() == Some("True")
                {
                    return None;
                }
                root_identity = get_attribute(&tag, "name").zip(get_attribute(&tag, "id"));
                depth += 1;
            }
            Event::Start(tag) | Event::Empty(tag)
                if depth == 1 && tag.name().as_ref() == "LayoutReference" =>
            {
                return get_attribute(&tag, "name")
                    .zip(get_attribute(&tag, "id"))
                    .or(root_identity)
                    .map(|(name, id)| (unescape_value(&name), unescape_value(&id)));
            }
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    root_identity.map(|(name, id)| (unescape_value(&name), unescape_value(&id)))
}

fn quote_field(value: &str, delimiter: char) -> String {
    if value.contains([delimiter, '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(extension: &str, tree: OutputTree) -> Flags {
        Flags {
            layouts_summary: Some(extension.to_string()),
            layouts_summary_file_name: None,
            change_directory_spaces: None,
            change_filename_spaces: None,
            remove_ddrrefs: false,
            parse_all_lines: false,
            lossless: false,
            output_tree: tree,
            obfuscate_passwords: false,
            script_extension: "txt".to_string(),
        }
    }

    #[test]
    fn summaries_use_current_files_and_actual_paths_in_both_trees() {
        for (tree, tree_name) in [(OutputTree::Db, "db"), (OutputTree::Domain, "domain")] {
            let root = std::env::temp_dir().join(format!(
                "fm-layout-summary-{tree_name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            let layouts = build_tree_path(&root, &tree, "layouts", "DB");
            fs::create_dir_all(layouts.join("Folder-ID-1")).unwrap();
            let output = OutputFiles::default();
            output.write(
                &layouts.join("Folder-ID-1/Sales-ID-9.xml"),
                r#"<Layout id="9" name="Sales, &quot;West&quot;"></Layout>"#,
            );
            output.write(
                &layouts.join("Home-ID-2.xml"),
                r#"<Layout id="2" name="Home"></Layout>"#,
            );
            output.write(
                &layouts.join("Folder-ID-1.xml"),
                r#"<Layout id="1" name="Folder" isFolder="True"></Layout>"#,
            );
            fs::write(
                layouts.join("stale.xml"),
                r#"<Layout id="99" name="Stale"></Layout>"#,
            )
            .unwrap();
            let flags = flags("csv", tree);
            write_summaries(&root, "DB", &flags, &output).unwrap();
            let summary = layouts.join(".summary.csv");
            assert_eq!(
                fs::read_to_string(&summary).unwrap(),
                "Name,ID,Path,File\nHome,2,,Home-ID-2.xml\n\"Sales, \"\"West\"\"\",9,Folder-ID-1,Sales-ID-9.xml\n"
            );
            output.remove_stale(&root).unwrap();
            assert!(summary.exists());
            assert!(!layouts.join("stale.xml").exists());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn custom_path_combines_databases_and_supports_tab_formats() {
        let root = std::env::temp_dir().join(format!("fm-layout-external-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("output");
        let output = OutputFiles::default();
        for db in ["A", "B"] {
            let dir = target.join(db).join("layouts");
            fs::create_dir_all(&dir).unwrap();
            output.write(
                &dir.join("Home.xml"),
                r#"<Layout><LayoutReference id="7" name="Home"></LayoutReference></Layout>"#,
            );
        }
        for extension in ["tab", "txt"] {
            let mut flags = flags(extension, OutputTree::Db);
            let summary = root
                .join("outside repository/nested")
                .join(format!("index.{extension}"));
            flags.layouts_summary_file_name = Some(summary.clone());
            write_summaries(&target, "", &flags, &output).unwrap();
            let text = fs::read_to_string(summary).unwrap();
            assert!(text.starts_with("Name\tID\tPath\tFile\n"));
            assert_eq!(text.lines().count(), 3);
            assert!(text.contains(&target.join("A/layouts").to_string_lossy().to_string()));
            assert!(text.contains(&target.join("B/layouts").to_string_lossy().to_string()));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delimited_fields_quote_tabs_and_newlines() {
        assert_eq!(quote_field("a\tb\n\"c\"", '\t'), "\"a\tb\n\"\"c\"\"\"");
    }
}
