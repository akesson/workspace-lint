// KNOWN FALSE POSITIVE: this file has only two lines of real logic, but the
// big embedded raw-string data blob below is counted by tokei as `code`, so
// file-size flags it as oversized (max-code-lines = 5). If tokei ever stops
// counting string-literal lines as code, this case stops firing and should be
// reclassified.
pub const DATA: &str = r#"
line 01 of embedded data
line 02 of embedded data
line 03 of embedded data
line 04 of embedded data
line 05 of embedded data
line 06 of embedded data
line 07 of embedded data
line 08 of embedded data
line 09 of embedded data
line 10 of embedded data
"#;
