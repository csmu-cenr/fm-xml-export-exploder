use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::config::Flags;
use crate::utils::file_utils::{escape_filename, join_scope_id_and_name, should_skip_line};
use crate::utils::output_files::OutputFiles;
use crate::utils::xml_utils::extract_values_from_xml_paths;

pub(crate) mod attributes;
pub(crate) mod entity;
pub(crate) mod file_utils;
pub(crate) mod output_files;
pub(crate) mod output_path;
pub(crate) mod skeleton;
pub(crate) mod xml_node;
pub(crate) mod xml_utils;

// Re-exports for backward compatibility
pub use entity::read_rest_of_element;
pub(crate) use output_path::VERSION_2_2_3_4;
pub use output_path::{
    build_out_dir_path, migrate_old_custom_functions_if_needed, output_roots,
    version_string_to_number,
};
pub use skeleton::push_line_to_skeleton;

/// Represents the folder structure for catalog items that utilize folders, e.g. custom functions, scripts, layouts
#[derive(Debug, Clone, Default)]
pub struct FolderStructure {
    /// Maps ID to the folder path where it should be placed
    pub item_paths: HashMap<String, Vec<String>>,
}

impl FolderStructure {
    /// Get the folder path for a specific ID
    pub fn get_path_for_id(&self, id: &str) -> &[String] {
        self.item_paths.get(id).map_or(&[], |v| v.as_slice())
    }
}

/// Some catalog items derive their file name from their content instead of the id and
/// name attributes of the catalog item tag, e.g. accounts are named after the account name.
/// The name may contain one folder, e.g. modified fields are grouped by their table.
pub fn derived_file_name(xml: &str, path_stack: &[String], tag_name: &str) -> Option<String> {
    if path_stack.get(1).is_none_or(|v| v != "Structure") {
        return None;
    }
    let action = path_stack.get(2).map(|v| v.as_str());

    // A modify action has one field catalog per field, so several of them share a table
    if (action, tag_name) == (Some("ModifyAction"), "FieldCatalog") {
        let results = extract_values_from_xml_paths(
            xml,
            &[
                "FieldCatalog/BaseTableReference/@name",
                "FieldCatalog/BaseTableReference/@id",
                "FieldCatalog/ObjectList/Field/FieldReference/@name",
                "FieldCatalog/ObjectList/Field/FieldReference/@id",
                "FieldCatalog/ObjectList/Field/FieldReference/BaseTableReference/@name",
                "FieldCatalog/ObjectList/Field/FieldReference/BaseTableReference/@id",
            ],
        )
        .ok()?;
        let [
            table,
            table_id,
            Some(field),
            Some(field_id),
            nested_table,
            nested_table_id,
        ] = results.as_slice()
        else {
            return None;
        };
        let (table, table_id) = match (table, table_id) {
            (Some(name), Some(id)) => (name, id),
            _ => match (nested_table, nested_table_id) {
                (Some(name), Some(id)) => (name, id),
                _ => return None,
            },
        };
        return Some(format!(
            "{}/{}.xml",
            escape_filename(&join_scope_id_and_name(table_id, table)),
            escape_filename(&join_scope_id_and_name(field_id, field))
        ));
    }

    let paths: &[&str] = match (action, tag_name) {
        (Some("AddAction"), "Account") => &["Account/Authentication/AccountName", "Account/@id"],
        (Some("AddAction"), "Authorization") => &["Authorization/Display", "Authorization/@id"],
        (Some("AddAction"), "BinaryData") => &[
            "BinaryData/LibraryReference/@key",
            "BinaryData/LibraryReference/@id",
        ],
        (Some("ModifyAction"), "Layout") => {
            &["Layout/LayoutReference/@name", "Layout/LayoutReference/@id"]
        }
        (Some("AddAction"), "Relationship") => &[
            "Relationship/LeftTable/TableOccurrenceReference/@name",
            "Relationship/RightTable/TableOccurrenceReference/@name",
            "Relationship/@id",
        ],
        _ => return None,
    };

    let results = extract_values_from_xml_paths(xml, paths).ok()?;
    let id = results.last()?.as_deref()?;

    let name_parts: Vec<&str> = results[..results.len() - 1]
        .iter()
        .filter_map(|r| r.as_deref())
        .collect();

    let mut file_name = name_parts.join(" - ");
    if !name_parts.is_empty() {
        file_name.push_str(" - ");
    }
    file_name.push_str("ID ");
    file_name.push_str(id);
    file_name.push_str(".xml");
    Some(escape_filename(&file_name))
}

pub fn create_dir(dir_path: &Path) {
    fs::create_dir_all(dir_path)
        .unwrap_or_else(|err| panic!("Error creating directory {}: {}", dir_path.display(), err));
}

/// Prepares exploded XML for writing: removes the given indentation and, unless all
/// lines are kept, noisy lines. Lines that continue a CDATA section or a text value
/// are content, so they keep their tabs, and carriage returns are kept as well.
pub fn prepare_xml_content(content: &str, remove_indent_count: usize, flags: &Flags) -> String {
    let filtered;
    let content = if flags.remove_ddrrefs {
        filtered = xml_utils::remove_ddrrefs(content);
        filtered.as_str()
    } else {
        content
    };
    let indent_prefix = "\t".repeat(remove_indent_count);
    let filter_noisy_lines = !flags.parse_all_lines && !flags.lossless;
    let mut file_content = String::with_capacity(content.len());
    let mut state = LineState::Markup;
    for line in content.split_terminator('\n') {
        let is_markup = is_markup_line(line, state);
        state = state_after_line(line, state);
        if filter_noisy_lines && should_skip_line(line) {
            continue;
        }
        let line = if is_markup {
            line.strip_prefix(&indent_prefix).unwrap_or(line)
        } else {
            line
        };
        file_content.push_str(line);
        file_content.push('\n');
    }
    file_content
}

/// Where a line of XML starts or ends: between tags, after a start tag that ended the
/// line, inside a text value or inside a CDATA section
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum LineState {
    Markup,
    Open,
    Text,
    CData,
}

/// Whether a line starting in `state` is indented markup rather than content
pub(crate) fn is_markup_line(line: &str, state: LineState) -> bool {
    let trimmed = line.trim_start();
    let is_tag = trimmed.starts_with('<') && !trimmed.starts_with("<![CDATA[");
    match state {
        LineState::Markup => is_tag,
        // An end tag right after its start tag closes a text value of line breaks
        LineState::Open => is_tag && !trimmed.starts_with("</"),
        LineState::Text | LineState::CData => false,
    }
}

/// The state at the end of a line that starts in `state`
pub(crate) fn state_after_line(line: &str, mut state: LineState) -> LineState {
    let mut rest = line;
    loop {
        if state == LineState::CData {
            let Some(end) = rest.find("]]>") else {
                return LineState::CData;
            };
            rest = &rest[end + 3..];
            state = LineState::Text;
            continue;
        }
        let Some(start) = rest.find('<') else {
            // Text without a tag on its own line continues a text value
            return if state != LineState::Markup || !rest.trim().is_empty() {
                LineState::Text
            } else {
                LineState::Markup
            };
        };
        rest = &rest[start..];
        if let Some(cdata) = rest.strip_prefix("<![CDATA[") {
            rest = cdata;
            state = LineState::CData;
            continue;
        }
        let Some(end) = tag_end(rest) else {
            return LineState::Markup;
        };
        let is_start_tag = !rest.starts_with("</")
            && !rest.starts_with("<?")
            && !rest.starts_with("<!")
            && !rest[..end].ends_with('/');
        rest = &rest[end + 1..];
        // A start tag at the end of a line is followed by indented children, or by the
        // rest of a text value that starts with a line break
        if is_start_tag && rest.trim().is_empty() {
            return LineState::Open;
        }
        state = if is_start_tag {
            LineState::Text
        } else {
            LineState::Markup
        };
    }
}

/// The index of the `>` that closes the tag at the start of `tag`, skipping quoted
/// attribute values
fn tag_end(tag: &str) -> Option<usize> {
    let mut quote = None;
    for (i, byte) in tag.bytes().enumerate() {
        match (quote, byte) {
            (None, b'"' | b'\'') => quote = Some(byte),
            (Some(q), _) if q == byte => quote = None,
            (None, b'>') => return Some(i),
            _ => {}
        }
    }
    None
}

pub fn write_xml_file(
    output: &OutputFiles,
    output_file_path: &Path,
    content: &str,
    remove_indent_count: usize,
    flags: &Flags,
) {
    output.write(
        &filename_with_replaced_spaces(output_file_path, flags),
        &prepare_xml_content(content, remove_indent_count, flags),
    );
}

static LINE_SPLIT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\r\n|\n\r|\r|\n").unwrap());

pub fn write_text_file(output: &OutputFiles, output_file_path: &Path, content: &str) {
    let mut file_content = String::with_capacity(content.len());
    for line in LINE_SPLIT_REGEX.split(content) {
        file_content.push_str(line);
        file_content.push('\n');
    }

    output.write(output_file_path, &file_content);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: &str = r#"<Account id="1" kind="0" type="FileMaker" enable="False">
	<UUID modifications="1" userName="Mislav" accountName="Admin" timestamp="2025-04-16T18:22:20">28F730CD-2774-443F-AC9E-A46818A3C05F</UUID>
	<TagList></TagList>
	<Description></Description>
	<Authentication>
		<AccountName>[Guest]</AccountName>
		<PasswordEncrypted>BUI6dktVvYySpQ==</PasswordEncrypted>
	</Authentication>
	<PrivilegeSetReference id="3" name="[Read-Only Access]"></PrivilegeSetReference>
</Account>"#;

    fn path_stack(section: &str) -> Vec<String> {
        Vec::from([
            "SomeElement".to_owned(),
            section.to_owned(),
            "AddAction".to_owned(),
        ])
    }

    #[test]
    fn test_derived_file_name() {
        assert_eq!(
            derived_file_name(ACCOUNT, &path_stack("Structure"), "Account").as_deref(),
            Some("[Guest] - ID 1.xml")
        );
    }

    #[test]
    fn derived_file_names_are_escaped() {
        let account = ACCOUNT.replace("[Guest]", "Sales/Admin: \"Test\"");
        assert_eq!(
            derived_file_name(&account, &path_stack("Structure"), "Account").as_deref(),
            Some("Sales_Admin_ _Test_ - ID 1.xml")
        );
    }

    #[test]
    fn modified_fields_are_named_after_their_table_and_field() {
        let field_catalog = r#"<FieldCatalog>
	<BaseTableReference id="129" name="Test/Table"></BaseTableReference>
	<ObjectList membercount="1">
		<Field>
			<FieldReference id="12" name="Calc?" repetition="1"></FieldReference>
		</Field>
	</ObjectList>
</FieldCatalog>"#;
        let modify_action = vec![
            "FMSaveAsXML".to_owned(),
            "Structure".to_owned(),
            "ModifyAction".to_owned(),
        ];
        assert_eq!(
            derived_file_name(field_catalog, &modify_action, "FieldCatalog").as_deref(),
            Some("Test_Table - ID 129/Calc_ - ID 12.xml")
        );
        // Added fields keep one file per table
        assert_eq!(
            derived_file_name(field_catalog, &path_stack("Structure"), "FieldCatalog"),
            None
        );
    }

    #[test]
    fn test_derived_file_name_wrong_path_stack() {
        assert_eq!(
            derived_file_name(ACCOUNT, &path_stack("WrongElement"), "Account"),
            None
        );
    }

    fn lossless() -> Flags {
        Flags {
            change_directory_spaces: None,
            change_filename_spaces: None,
            remove_ddrrefs: false,
            parse_all_lines: false,
            lossless: true,
            output_tree: crate::config::OutputTree::Db,
            obfuscate_passwords: false,
            script_extension: "txt".to_string(),
        }
    }

    #[test]
    fn content_lines_keep_their_indentation() {
        let xml = "\t\t<Calculation>\n\t\t\t<Text><![CDATA[Let ( [\n\t\t\t\tx = 1\n] ; x )]]></Text>\n\t\t\t<Chunk>a\n\t\t\t\tb</Chunk>\n\t\t</Calculation>\n";
        assert_eq!(
            prepare_xml_content(xml, 2, &lossless()),
            "<Calculation>\n\t<Text><![CDATA[Let ( [\n\t\t\t\tx = 1\n] ; x )]]></Text>\n\t<Chunk>a\n\t\t\t\tb</Chunk>\n</Calculation>\n"
        );
    }

    #[test]
    fn text_values_of_line_breaks_are_content() {
        let xml = "\t\t<Value>\n\t\t\t<Data>\n</Data>\n\t\t\t<Data>\n\t\n\t</Data>\n\t\t</Value>\n";
        assert_eq!(
            prepare_xml_content(xml, 2, &lossless()),
            "<Value>\n\t<Data>\n</Data>\n\t<Data>\n\t\n\t</Data>\n</Value>\n"
        );
    }

    #[test]
    fn carriage_returns_are_kept() {
        let xml = "\t<Text><![CDATA[Not (\r\n\tx\r\n)]]></Text>\n";
        assert_eq!(
            prepare_xml_content(xml, 1, &lossless()),
            "<Text><![CDATA[Not (\r\n\tx\r\n)]]></Text>\n"
        );
    }

    #[test]
    fn quoted_angle_brackets_do_not_end_a_tag() {
        let xml = "\t<Field name=\"a > b\">\n\t\t<Comment></Comment>\n\t</Field>\n";
        assert_eq!(
            prepare_xml_content(xml, 1, &lossless()),
            "<Field name=\"a > b\">\n\t<Comment></Comment>\n</Field>\n"
        );
    }

    #[test]
    fn test_derived_file_name_other_item() {
        assert_eq!(
            derived_file_name(ACCOUNT, &path_stack("Structure"), "Script"),
            None
        );
    }
}

/// Replace spaces and collapse runs of replacement characters in the output filename.
pub(crate) fn filename_with_replaced_spaces(output_file_path: &Path, flags: &Flags) -> PathBuf {
    let mut path = output_file_path.to_path_buf();
    if let Some(replacement) = &flags.change_filename_spaces
        && let Some(name) = path.file_name().and_then(|name| name.to_str())
    {
        path.set_file_name(replace_name_spaces(name, replacement));
    }
    path
}

/// Shared replacement rules for filenames and generated directory components.
fn replace_name_spaces(name: &str, replacement: &str) -> String {
    static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(" +").unwrap());
    let normalized = SPACES.replace_all(name, " ");
    let name = normalized.replace(' ', replacement);
    let mut collapsed = String::with_capacity(name.len());
    let mut previous = None;
    for character in name.chars() {
        if previous != Some(character) || !replacement.contains(character) {
            collapsed.push(character);
        }
        previous = Some(character);
    }
    collapsed
}

pub(crate) fn directory_name_with_replaced_spaces(name: &str, flags: &Flags) -> String {
    let Some(replacement) = &flags.change_directory_spaces else {
        return name.to_owned();
    };
    let replaced = replace_name_spaces(name, replacement);
    // A generated name must remain a directory component.
    if matches!(replaced.as_str(), "" | "." | "..") {
        name.to_owned()
    } else {
        replaced
    }
}

/// Normalize only generated directories below `root`, preserving the supplied target path.
pub(crate) fn directories_with_replaced_spaces(root: &Path, path: &Path, flags: &Flags) -> PathBuf {
    let relative = path
        .strip_prefix(root)
        .expect("Generated path must be below its output root");
    let mut result = root.to_path_buf();
    for component in relative.components() {
        let name = component.as_os_str();
        match name.to_str() {
            Some(name) => result.push(directory_name_with_replaced_spaces(name, flags)),
            None => result.push(name),
        }
    }
    result
}

#[cfg(test)]
mod filename_space_tests {
    use super::*;
    use crate::config::OutputTree;

    #[test]
    fn directory_spaces_preserve_target_and_filename() {
        let mut flags = Flags {
            change_directory_spaces: Some("--".to_owned()),
            change_filename_spaces: None,
            remove_ddrrefs: false,
            parse_all_lines: false,
            lossless: false,
            output_tree: OutputTree::Db,
            obfuscate_passwords: false,
            script_extension: "txt".to_owned(),
        };
        let root = Path::new("target with spaces");
        let directories = root.join("Folder  -- Name - ID 1/Nested   Folder - ID 2");
        let result = directories_with_replaced_spaces(root, &directories, &flags);
        assert_eq!(result, root.join("Folder-Name-ID-1/Nested-Folder-ID-2"));
        let file = result.join("Script Name - ID 3.fmscript");
        assert_eq!(filename_with_replaced_spaces(&file, &flags), file);
        flags.change_directory_spaces = Some("$1".to_owned());
        assert_eq!(directory_name_with_replaced_spaces("A  B", &flags), "A$1B");
        flags.change_directory_spaces = None;
        assert_eq!(
            directories_with_replaced_spaces(root, &directories, &flags),
            directories
        );
    }

    #[test]
    fn filename_spaces_collapse_before_literal_replacement() {
        let mut flags = Flags {
            parse_all_lines: false,
            lossless: false,
            output_tree: OutputTree::Db,
            obfuscate_passwords: false,
            change_directory_spaces: None,
            change_filename_spaces: Some("-".to_owned()),
            remove_ddrrefs: false,
            script_extension: ".txt".to_owned(),
        };
        let path = Path::new("folder with spaces/Script  with   spaces.txt");
        assert_eq!(
            filename_with_replaced_spaces(path, &flags),
            PathBuf::from("folder with spaces/Script-with-spaces.txt")
        );
        let separators = Path::new("folder with spaces/Book  -- Title - ID  1.xml");
        assert_eq!(
            filename_with_replaced_spaces(separators, &flags),
            PathBuf::from("folder with spaces/Book-Title-ID-1.xml")
        );
        assert_eq!(
            filename_with_replaced_spaces(
                &filename_with_replaced_spaces(separators, &flags),
                &flags
            ),
            filename_with_replaced_spaces(separators, &flags)
        );
        flags.change_filename_spaces = Some("_".to_owned());
        assert_eq!(
            filename_with_replaced_spaces(Path::new("Book __ Title.txt"), &flags),
            PathBuf::from("Book_Title.txt")
        );
        flags.change_filename_spaces = Some("--".to_owned());
        assert_eq!(
            filename_with_replaced_spaces(path, &flags),
            PathBuf::from("folder with spaces/Script-with-spaces.txt")
        );
        flags.change_filename_spaces = Some("$1".to_owned());
        assert_eq!(
            filename_with_replaced_spaces(path, &flags),
            PathBuf::from("folder with spaces/Script$1with$1spaces.txt")
        );
        flags.change_filename_spaces = None;
        assert_eq!(filename_with_replaced_spaces(path, &flags), path);
    }
}
