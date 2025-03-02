use super::util;
use ansi_term::Colour;
use anyhow::{anyhow, Context, Result};
use difference::{Changeset, Difference};
use lazy_static::lazy_static;
use regex::bytes::{Regex as ByteRegex, RegexBuilder as ByteRegexBuilder};
use regex::Regex;
use std::ffi::OsStr;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str;
use tree_sitter::{Language, LogType, Parser, Query};
use walkdir::WalkDir;

lazy_static! {
    static ref FIRST_HEADER_REGEX: ByteRegex =
        ByteRegexBuilder::new(r"^===+(?P<suffix>[^=\r\n]*)\r?\n")
            .multi_line(true)
            .build()
            .unwrap();
    static ref HEADER_REGEX: ByteRegex =
        ByteRegexBuilder::new(r"^===+\r?\n(?P<test_name>[^=\r\n]*)\r?\n===+\r?\n")
            .multi_line(true)
            .build()
            .unwrap();
    static ref DIVIDER_REGEX: ByteRegex = ByteRegexBuilder::new(r"^---+\r?\n")
        .multi_line(true)
        .build()
        .unwrap();
    static ref COMMENT_REGEX: Regex = Regex::new(r"(?m)^\s*;.*$").unwrap();
    static ref WHITESPACE_REGEX: Regex = Regex::new(r"\s+").unwrap();
    static ref SEXP_FIELD_REGEX: Regex = Regex::new(r" \w+: \(").unwrap();
}

#[derive(Debug, PartialEq, Eq)]
pub enum TestEntry {
    Group {
        name: String,
        children: Vec<TestEntry>,
        file_path: Option<PathBuf>,
    },
    Example {
        name: String,
        input: Vec<u8>,
        output: String,
        has_fields: bool,
    },
}

impl Default for TestEntry {
    fn default() -> Self {
        TestEntry::Group {
            name: String::new(),
            children: Vec::new(),
            file_path: None,
        }
    }
}

pub fn run_tests_at_path(
    language: Language,
    path: &Path,
    debug: bool,
    debug_graph: bool,
    filter: Option<&str>,
    update: bool,
) -> Result<()> {
    let test_entry = parse_tests(path)?;
    let mut _log_session = None;
    let mut parser = Parser::new();
    parser.set_language(language)?;

    if debug_graph {
        _log_session = Some(util::log_graphs(&mut parser, "log.html")?);
    } else if debug {
        parser.set_logger(Some(Box::new(|log_type, message| {
            if log_type == LogType::Lex {
                io::stderr().write(b"  ").unwrap();
            }
            write!(&mut io::stderr(), "{}\n", message).unwrap();
        })));
    }

    let mut failures = Vec::new();
    let mut corrected_entries = Vec::new();
    run_tests(
        &mut parser,
        test_entry,
        filter,
        0,
        &mut failures,
        update,
        &mut corrected_entries,
    )?;

    if failures.len() > 0 {
        println!("");

        if update {
            if failures.len() == 1 {
                println!("1 update:\n")
            } else {
                println!("{} updates:\n", failures.len())
            }

            for (i, (name, ..)) in failures.iter().enumerate() {
                println!("  {}. {}", i + 1, name);
            }
            Ok(())
        } else {
            if failures.len() == 1 {
                println!("1 failure:")
            } else {
                println!("{} failures:", failures.len())
            }

            print_diff_key();
            for (i, (name, actual, expected)) in failures.iter().enumerate() {
                println!("\n  {}. {}:", i + 1, name);
                let actual = format_sexp_indented(&actual, 2);
                let expected = format_sexp_indented(&expected, 2);
                print_diff(&actual, &expected);
            }
            Err(anyhow!(""))
        }
    } else {
        Ok(())
    }
}

pub fn check_queries_at_path(language: Language, path: &Path) -> Result<()> {
    if path.exists() {
        for entry in WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file()
                    && e.path().extension().and_then(OsStr::to_str) == Some("scm")
                    && !e.path().starts_with(".")
            })
        {
            let filepath = entry.file_name().to_str().unwrap_or("");
            let content = fs::read_to_string(entry.path())
                .with_context(|| format!("Error reading query file {:?}", filepath))?;
            Query::new(language, &content)
                .with_context(|| format!("Error in query file {:?}", filepath))?;
        }
    }
    Ok(())
}

pub fn print_diff_key() {
    println!(
        "\n{} / {}",
        Colour::Green.paint("expected"),
        Colour::Red.paint("actual")
    );
}

pub fn print_diff(actual: &String, expected: &String) {
    let changeset = Changeset::new(actual, expected, "\n");
    for diff in &changeset.diffs {
        match diff {
            Difference::Same(part) => {
                print!("{}{}", part, changeset.split);
            }
            Difference::Add(part) => {
                print!("{}{}", Colour::Green.paint(part), changeset.split);
            }
            Difference::Rem(part) => {
                print!("{}{}", Colour::Red.paint(part), changeset.split);
            }
        }
    }
    println!("");
}

fn run_tests(
    parser: &mut Parser,
    test_entry: TestEntry,
    filter: Option<&str>,
    mut indent_level: i32,
    failures: &mut Vec<(String, String, String)>,
    update: bool,
    corrected_entries: &mut Vec<(String, String, String)>,
) -> Result<()> {
    match test_entry {
        TestEntry::Example {
            name,
            input,
            output,
            has_fields,
        } => {
            if let Some(filter) = filter {
                if !name.contains(filter) {
                    if update {
                        let input = String::from_utf8(input).unwrap();
                        let output = format_sexp(&output);
                        corrected_entries.push((name, input, output));
                    }
                    return Ok(());
                }
            }
            let tree = parser.parse(&input, None).unwrap();
            let mut actual = tree.root_node().to_sexp();
            if !has_fields {
                actual = strip_sexp_fields(actual);
            }
            for _ in 0..indent_level {
                print!("  ");
            }
            if actual == output {
                println!("✓ {}", Colour::Green.paint(&name));
                if update {
                    let input = String::from_utf8(input).unwrap();
                    let output = format_sexp(&output);
                    corrected_entries.push((name, input, output));
                }
            } else {
                if update {
                    let input = String::from_utf8(input).unwrap();
                    let output = format_sexp(&actual);
                    corrected_entries.push((name.clone(), input, output));
                    println!("✓ {}", Colour::Blue.paint(&name));
                } else {
                    println!("✗ {}", Colour::Red.paint(&name));
                }
                failures.push((name, actual, output));
            }
        }
        TestEntry::Group {
            name,
            children,
            file_path,
        } => {
            if indent_level > 0 {
                for _ in 0..indent_level {
                    print!("  ");
                }
                println!("{}:", name);
            }

            let failure_count = failures.len();

            indent_level += 1;
            for child in children {
                run_tests(
                    parser,
                    child,
                    filter,
                    indent_level,
                    failures,
                    update,
                    corrected_entries,
                )?;
            }

            if let Some(file_path) = file_path {
                if update && failures.len() - failure_count > 0 {
                    write_tests(&file_path, corrected_entries)?;
                }
                corrected_entries.clear();
            }
        }
    }
    Ok(())
}

fn format_sexp(sexp: &String) -> String {
    format_sexp_indented(sexp, 0)
}

fn format_sexp_indented(sexp: &String, initial_indent_level: u32) -> String {
    let mut formatted = String::new();

    let mut indent_level = initial_indent_level;
    let mut has_field = false;
    let mut s_iter = sexp.split(|c| c == ' ' || c == ')');
    while let Some(s) = s_iter.next() {
        if s.is_empty() {
            // ")"
            indent_level -= 1;
            write!(formatted, ")").unwrap();
        } else if s.starts_with('(') {
            if has_field {
                has_field = false;
            } else {
                if indent_level > 0 {
                    writeln!(formatted, "").unwrap();
                    for _ in 0..indent_level {
                        write!(formatted, "  ").unwrap();
                    }
                }
                indent_level += 1;
            }

            // "(node_name"
            write!(formatted, "{}", s).unwrap();

            let mut c_iter = s.chars();
            c_iter.next();
            let second_char = c_iter.next().unwrap();
            if second_char == 'M' || second_char == 'U' {
                // "(MISSING node_name" or "(UNEXPECTED 'x'"
                let s = s_iter.next().unwrap();
                write!(formatted, " {}", s).unwrap();
            }
        } else if s.ends_with(':') {
            // "field:"
            writeln!(formatted, "").unwrap();
            for _ in 0..indent_level {
                write!(formatted, "  ").unwrap();
            }
            write!(formatted, "{} ", s).unwrap();
            has_field = true;
            indent_level += 1;
        }
    }

    formatted
}

fn write_tests(file_path: &Path, corrected_entries: &Vec<(String, String, String)>) -> Result<()> {
    let mut buffer = fs::File::create(file_path)?;
    write_tests_to_buffer(&mut buffer, corrected_entries)
}

fn write_tests_to_buffer(
    buffer: &mut impl Write,
    corrected_entries: &Vec<(String, String, String)>,
) -> Result<()> {
    for (i, (name, input, output)) in corrected_entries.iter().enumerate() {
        if i > 0 {
            write!(buffer, "\n")?;
        }
        write!(
            buffer,
            "{}\n{}\n{}\n{}\n{}\n\n{}\n",
            "=".repeat(80),
            name,
            "=".repeat(80),
            input,
            "-".repeat(80),
            output.trim()
        )?;
    }
    Ok(())
}

pub fn parse_tests(path: &Path) -> io::Result<TestEntry> {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    if path.is_dir() {
        let mut children = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let hidden = entry.file_name().to_str().unwrap_or("").starts_with(".");
            if !hidden {
                children.push(parse_tests(&entry.path())?);
            }
        }
        Ok(TestEntry::Group {
            name,
            children,
            file_path: None,
        })
    } else {
        let content = fs::read_to_string(path)?;
        Ok(parse_test_content(name, content, Some(path.to_path_buf())))
    }
}

pub fn strip_sexp_fields(sexp: String) -> String {
    SEXP_FIELD_REGEX.replace_all(&sexp, " (").to_string()
}

fn parse_test_content(name: String, content: String, file_path: Option<PathBuf>) -> TestEntry {
    let mut children = Vec::new();
    let bytes = content.as_bytes();
    let mut prev_name = String::new();
    let mut prev_header_end = 0;

    let suffix = FIRST_HEADER_REGEX
        .captures(bytes)
        .and_then(|c| c.name("suffix"))
        .map(|m| &bytes[m.range()])
        .map(|b| String::from_utf8_lossy(b).to_string())
        .map(|s| regex::escape(&s[..]));

    let suffix_header_pattern: Option<String> = suffix.as_ref().map(|s| {
        String::from(r"^===+") + s + r"\r?\n(?P<test_name>[^\r\n]*)\r?\n===+" + s + r"\r?\n"
    });

    let header_regex_from_suffix_header_pattern = suffix_header_pattern
        .as_ref()
        .and_then(|s| ByteRegexBuilder::new(&s[..]).multi_line(true).build().ok());

    let header_regex = header_regex_from_suffix_header_pattern
        .as_ref()
        .unwrap_or(&HEADER_REGEX);

    let suffix_divider_pattern: Option<String> = suffix
        .as_ref()
        .map(|s| String::from(r"^---+") + s + r"\r?\n");

    let divider_regex_from_suffix_divider_pattern = suffix_divider_pattern
        .as_ref()
        .and_then(|s| ByteRegexBuilder::new(&s[..]).multi_line(true).build().ok());

    let divider_regex = divider_regex_from_suffix_divider_pattern
        .as_ref()
        .unwrap_or(&DIVIDER_REGEX);

    // Identify all of the test descriptions using the `======` headers.
    // Must be followed by custom suffix if defined on first header.
    // Capture index 0 corresponds to entire match and is guaranteed to exist.
    for (header_start, header_end, test_name_capture) in header_regex
        .captures_iter(&bytes)
        .map(|c| {
            (
                c.get(0).unwrap().start(),
                c.get(0).unwrap().end(),
                c.name("test_name"),
            )
        })
        .chain(Some((bytes.len(), bytes.len(), None)))
    {
        // Find the longest line of dashes following each test description.
        // That is the divider between input and expected output.
        // Must be followed by custom suffix if defined on first header.
        if prev_header_end > 0 {
            let divider_match = divider_regex
                .find_iter(&bytes[prev_header_end..header_start])
                .map(|m| (prev_header_end + m.start(), prev_header_end + m.end()))
                .max_by_key(|(start, end)| end - start);
            if let Some((divider_start, divider_end)) = divider_match {
                if let Ok(output) = str::from_utf8(&bytes[divider_end..header_start]) {
                    let mut input = bytes[prev_header_end..divider_start].to_vec();

                    // Remove trailing newline from the input.
                    input.pop();
                    if input.last() == Some(&b'\r') {
                        input.pop();
                    }

                    // Remove all comments
                    let output = COMMENT_REGEX.replace_all(output, "").to_string();
                    // Normalize the whitespace in the expected output.
                    let output = WHITESPACE_REGEX.replace_all(output.trim(), " ");
                    let output = output.replace(" )", ")");

                    // Identify if the expected output has fields indicated. If not, then
                    // fields will not be checked.
                    let has_fields = SEXP_FIELD_REGEX.is_match(&output);

                    children.push(TestEntry::Example {
                        name: prev_name,
                        input,
                        output,
                        has_fields,
                    });
                }
            }
        }
        prev_name = test_name_capture
            .map(|m| &bytes[m.range()])
            .map(|b| String::from_utf8_lossy(b).to_string())
            .unwrap_or(String::new());
        prev_header_end = header_end;
    }
    TestEntry::Group {
        name,
        children,
        file_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_test_content() {
        let entry = parse_test_content(
            "the-filename".to_string(),
            r#"
===============
The first test
===============

a b c

---

(a
    (b c))

================
The second test
================
d
---
(d)
        "#
            .trim()
            .to_string(),
            None,
        );

        assert_eq!(
            entry,
            TestEntry::Group {
                name: "the-filename".to_string(),
                children: vec![
                    TestEntry::Example {
                        name: "The first test".to_string(),
                        input: "\na b c\n".as_bytes().to_vec(),
                        output: "(a (b c))".to_string(),
                        has_fields: false,
                    },
                    TestEntry::Example {
                        name: "The second test".to_string(),
                        input: "d".as_bytes().to_vec(),
                        output: "(d)".to_string(),
                        has_fields: false,
                    },
                ],
                file_path: None,
            }
        );
    }

    #[test]
    fn test_parse_test_content_with_dashes_in_source_code() {
        let entry = parse_test_content(
            "the-filename".to_string(),
            r#"
==================
Code with dashes
==================
abc
---
defg
----
hijkl
-------

(a (b))

=========================
Code ending with dashes
=========================
abc
-----------
-------------------

(c (d))
        "#
            .trim()
            .to_string(),
            None,
        );

        assert_eq!(
            entry,
            TestEntry::Group {
                name: "the-filename".to_string(),
                children: vec![
                    TestEntry::Example {
                        name: "Code with dashes".to_string(),
                        input: "abc\n---\ndefg\n----\nhijkl".as_bytes().to_vec(),
                        output: "(a (b))".to_string(),
                        has_fields: false,
                    },
                    TestEntry::Example {
                        name: "Code ending with dashes".to_string(),
                        input: "abc\n-----------".as_bytes().to_vec(),
                        output: "(c (d))".to_string(),
                        has_fields: false,
                    },
                ],
                file_path: None,
            }
        );
    }

    #[test]
    fn test_format_sexp() {
        assert_eq!(
            format_sexp(&"(a b: (c) (d) e: (f (g (h (MISSING i)))))".to_string()),
            r#"
(a
  b: (c)
  (d)
  e: (f
    (g
      (h
        (MISSING i)))))
"#
            .trim()
            .to_string()
        );
    }

    #[test]
    fn test_write_tests_to_buffer() {
        let mut buffer = Vec::new();
        let corrected_entries = vec![
            (
                "title 1".to_string(),
                "input 1".to_string(),
                "output 1".to_string(),
            ),
            (
                "title 2".to_string(),
                "input 2".to_string(),
                "output 2".to_string(),
            ),
        ];
        write_tests_to_buffer(&mut buffer, &corrected_entries).unwrap();
        assert_eq!(
            String::from_utf8(buffer).unwrap(),
            r#"
================================================================================
title 1
================================================================================
input 1
--------------------------------------------------------------------------------

output 1

================================================================================
title 2
================================================================================
input 2
--------------------------------------------------------------------------------

output 2
"#
            .trim_start()
            .to_string()
        );
    }

    #[test]
    fn test_parse_test_content_with_comments_in_sexp() {
        let entry = parse_test_content(
            "the-filename".to_string(),
            r#"
==================
sexp with comment
==================
code
---

; Line start comment
(a (b))

==================
sexp with comment between
==================
code
---

; Line start comment
(a 
; ignore this
    (b)
    ; also ignore this
)

=========================
sexp with ';'
=========================
code
---

(MISSING ";")
        "#
            .trim()
            .to_string(),
            None,
        );

        assert_eq!(
            entry,
            TestEntry::Group {
                name: "the-filename".to_string(),
                children: vec![
                    TestEntry::Example {
                        name: "sexp with comment".to_string(),
                        input: "code".as_bytes().to_vec(),
                        output: "(a (b))".to_string(),
                        has_fields: false,
                    },
                    TestEntry::Example {
                        name: "sexp with comment between".to_string(),
                        input: "code".as_bytes().to_vec(),
                        output: "(a (b))".to_string(),
                        has_fields: false,
                    },
                    TestEntry::Example {
                        name: "sexp with ';'".to_string(),
                        input: "code".as_bytes().to_vec(),
                        output: "(MISSING \";\")".to_string(),
                        has_fields: false,
                    }
                ],
                file_path: None,
            }
        );
    }

    #[test]
    fn test_parse_test_content_with_suffixes() {
        let entry = parse_test_content(
            "the-filename".to_string(),
            r#"
==================asdf\()[]|{}*+?^$.-
First test
==================asdf\()[]|{}*+?^$.-

=========================
NOT A TEST HEADER
=========================
-------------------------

---asdf\()[]|{}*+?^$.-

(a)

==================asdf\()[]|{}*+?^$.-
Second test
==================asdf\()[]|{}*+?^$.-

=========================
NOT A TEST HEADER
=========================
-------------------------

---asdf\()[]|{}*+?^$.-

(a)

=========================asdf\()[]|{}*+?^$.-
Test name with = symbol
=========================asdf\()[]|{}*+?^$.-

=========================
NOT A TEST HEADER
=========================
-------------------------

---asdf\()[]|{}*+?^$.-

(a)
        "#
            .trim()
            .to_string(),
            None,
        );

        let expected_input = "\n=========================\n\
            NOT A TEST HEADER\n\
            =========================\n\
            -------------------------\n"
            .as_bytes()
            .to_vec();
        assert_eq!(
            entry,
            TestEntry::Group {
                name: "the-filename".to_string(),
                children: vec![
                    TestEntry::Example {
                        name: "First test".to_string(),
                        input: expected_input.clone(),
                        output: "(a)".to_string(),
                        has_fields: false,
                    },
                    TestEntry::Example {
                        name: "Second test".to_string(),
                        input: expected_input.clone(),
                        output: "(a)".to_string(),
                        has_fields: false,
                    },
                    TestEntry::Example {
                        name: "Test name with = symbol".to_string(),
                        input: expected_input.clone(),
                        output: "(a)".to_string(),
                        has_fields: false,
                    }
                ],
                file_path: None,
            }
        );
    }
}
