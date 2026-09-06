//! Setting keys in someone else's INI file without disturbing the rest of it.
//!
//! Several components in this space are configured by an INI rather than by
//! which files are present: OptiScaler decides whether frame generation runs,
//! which upscaler it hooks and whether the neural pass is on entirely from
//! `OptiScaler.ini`. So an install that copies the right files and writes no
//! settings produces a component that loads and does nothing.
//!
//! # The whole difficulty is that the file is not ours
//!
//! A user who has been running OptiScaler has a tuned `OptiScaler.ini`: an
//! output-resolution dial, a sharpness value, a hotkey, comments they left
//! themselves. Rewriting it from a template is the kind of help that loses
//! somebody an evening, so this edits in place and touches nothing it was not
//! asked to.
//!
//! Specifically it preserves:
//!
//! - every line outside the section being edited, comments included;
//! - unrelated keys inside that section, and their order;
//! - the file's line endings - a Windows INI is CRLF, and rewriting it as LF
//!   is a whole-file diff for no reason;
//! - a commented-out key, which stays a comment. `;Enabled=false` is not a
//!   setting, and quietly turning it into one would be a surprise;
//! - the spelling, indentation and spacing of a key it rewrites, so a file
//!   that says `Enabled = auto` gets back `Enabled = true` and not
//!   `Enabled=true`. Four lines styled unlike the other sixteen hundred is a
//!   change nobody asked for, and it shows in every later diff.
//!
//! # Two things the reference implementation gets wrong
//!
//! DLSS5-Autopilot's `_ini_set` is the model for this, and it has two defects
//! that are worth not copying:
//!
//! 1. **A duplicated section corrupts the edit.** It records the *last*
//!    matching section as the start but the *first* following section as the
//!    end, so with `[A] [B] [A]` the end lands before the start and the new
//!    keys are inserted into `[A]`'s first copy - a different section from the
//!    one being written to. A duplicated section is malformed, and real files
//!    have them anyway.
//! 2. **It normalises line endings**, because it splits on any newline and
//!    joins with `\n`.
//!
//! Both are avoided here, and both have a test.

/// Sets `values` inside `section`, creating the section if it is absent.
///
/// Key matching is case-insensitive, because INI readers generally are, and a
/// key that is found keeps the user's own spelling, indentation and spacing -
/// only its value changes. A key that is added is written with the spacing the
/// rest of its section uses. The value is written verbatim.
///
/// `values` is a slice of pairs rather than a map so the order new keys are
/// added in is the caller's, and stays stable between runs.
pub fn set(text: &str, section: &str, values: &[(&str, &str)]) -> String {
    let newline = dominant_newline(text);
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();

    let (start, end) = find_section(&lines, section);

    let Some(start) = start else {
        // Absent, so append it. A blank line first if the file does not
        // already end in one, so the new section is not glued to the last key
        // of the previous one.
        if lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("[{section}]"));
        for (key, value) in values {
            lines.push(format!("{key}={value}"));
        }
        return join(&lines, newline);
    };

    // How this section spaces its assignments, so anything added matches. A
    // real OptiScaler.ini writes `Key = value`, and inserting `Key=value` into
    // it leaves a handful of lines that look unlike the other sixteen hundred.
    let separator = section_separator(&lines, start, end);

    // Rewrite the keys that are already there, and remember which were not.
    let mut pending: Vec<(&str, &str)> = values.to_vec();
    for line in lines.iter_mut().take(end).skip(start + 1) {
        let trimmed = line.trim();
        // A comment stays a comment, and a line with no `=` is not a setting.
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, _)) = trimmed.split_once('=') else {
            continue;
        };
        if let Some(index) = pending
            .iter()
            .position(|(key, _)| key.eq_ignore_ascii_case(name.trim()))
        {
            let (_, value) = pending.remove(index);
            // Everything up to and including the `=` is left exactly as the
            // user wrote it - their indentation, their capitalisation, their
            // spacing - and only the value is replaced. Rewriting the key too
            // would be a change we were not asked to make, and the reader is
            // case-insensitive anyway, so our spelling buys nothing.
            let eq = line.find('=').unwrap_or(0);
            let spacing = line[eq + 1..]
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect::<String>();
            *line = format!("{}{spacing}{value}", &line[..=eq]);
        }
    }

    // The rest go at the end of the section, before any blank lines that
    // separate it from the next one - so they join the section rather than
    // drifting into the gap after it.
    let mut at = end;
    while at > start + 1 && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    for (key, value) in pending {
        lines.insert(at, format!("{key}{separator}{value}"));
        at += 1;
    }

    join(&lines, newline)
}

/// Reads one key, or `None` when the section or the key is absent.
///
/// Used to check whether a setting we are about to write is already what we
/// want, so an install that changes nothing can say so rather than rewriting
/// the file and updating its timestamp.
pub fn get(text: &str, section: &str, key: &str) -> Option<String> {
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let (start, end) = find_section(&lines, section);
    let start = start?;

    lines.iter().take(end).skip(start + 1).find_map(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with(';') || trimmed.starts_with('#') {
            return None;
        }
        let (name, value) = trimmed.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case(key)
            .then(|| value.trim().to_owned())
    })
}

/// The bounds of a section: the header's index, and the index one past its
/// last line.
///
/// The end is found *relative to the chosen start* rather than as the first
/// header in the file, which is the bug described in the module docs. When a
/// section appears more than once the last copy wins, because that is what an
/// INI reader that overwrites as it parses would do.
fn find_section(lines: &[String], section: &str) -> (Option<usize>, usize) {
    let header = |line: &str| {
        let trimmed = line.trim();
        let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
        Some(inner.trim().to_owned())
    };

    let start = lines
        .iter()
        .enumerate()
        .rfind(|(_, line)| header(line).is_some_and(|name| name.eq_ignore_ascii_case(section)))
        .map(|(index, _)| index);

    let end = start.map_or(lines.len(), |start| {
        lines
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, line)| header(line).is_some())
            .map_or(lines.len(), |(index, _)| index)
    });

    (start, end)
}

/// Whichever line ending the file mostly uses, defaulting to the platform's.
///
/// Counted rather than sniffed from the first line: a file edited by two tools
/// can be mixed, and the majority is the least surprising answer. An empty or
/// single-line file has no evidence, and on Windows - where every file this
/// touches lives - CRLF is the right default.
/// How this section writes `key = value`, taken from its first assignment.
///
/// Returns the separator including whatever spacing surrounds the `=`, so a
/// key added to a section that writes `Key = value` is written the same way.
/// A section with no assignments to learn from gets the tight form, which is
/// what an INI written by a program looks like.
fn section_separator(lines: &[String], start: usize, end: usize) -> String {
    lines
        .iter()
        .take(end)
        .skip(start + 1)
        .find_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with(';') || trimmed.starts_with('#') {
                return None;
            }
            let (name, rest) = trimmed.split_once('=')?;
            let before: String = name
                .chars()
                .rev()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let after: String = rest
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            Some(format!("{before}={after}"))
        })
        .unwrap_or_else(|| "=".to_owned())
}

fn dominant_newline(text: &str) -> &'static str {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    if lf > crlf {
        "\n"
    } else {
        "\r\n"
    }
}

fn join(lines: &[String], newline: &str) -> String {
    let mut out = lines.join(newline);
    // Ending on a newline: an INI whose last line has none is valid but
    // appending to it later would join two keys into one.
    out.push_str(newline);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_that_is_there_is_rewritten_in_place() {
        let before = "[FrameGen]\r\nEnabled=false\r\nFGInput=upscaler\r\n";
        let after = set(before, "FrameGen", &[("Enabled", "true")]);
        assert_eq!(after, "[FrameGen]\r\nEnabled=true\r\nFGInput=upscaler\r\n");
    }

    #[test]
    fn a_tuned_file_keeps_everything_we_were_not_asked_about() {
        // The case this module exists for. Someone's own comments, their own
        // values, and a section we have no business touching.
        let before = concat!(
            "; my settings - do not lose these\r\n",
            "[Upscalers]\r\n",
            "Dx12Upscaler=dlss\r\n",
            "\r\n",
            "[FrameGen]\r\n",
            "; left off deliberately last time\r\n",
            "Enabled=false\r\n",
            "\r\n",
            "[Sharpness]\r\n",
            "Enabled=true\r\n",
            "Sharpness=0.35\r\n",
        );
        let after = set(before, "FrameGen", &[("Enabled", "true")]);

        assert!(after.contains("; my settings - do not lose these"));
        assert!(after.contains("Dx12Upscaler=dlss"));
        assert!(after.contains("; left off deliberately last time"));
        assert!(after.contains("Sharpness=0.35"));
        // The other section's `Enabled` is untouched, so both are still there.
        assert_eq!(after.matches("Enabled=true").count(), 2);
        assert!(!after.contains("Enabled=false"));
    }

    #[test]
    fn a_commented_out_key_stays_a_comment() {
        // `;Enabled=false` is not a setting. Matching it would turn a line the
        // user disabled into one we enabled, which is a surprise rather than a
        // fix - and it would leave the section with no active key at all if we
        // then thought the work was done.
        let before = "[OptiFG]\n;HUDFix=false\n";
        let after = set(before, "OptiFG", &[("HUDFix", "true")]);
        assert!(after.contains(";HUDFix=false"), "{after}");
        assert!(after.contains("\nHUDFix=true"), "{after}");
    }

    #[test]
    fn a_missing_key_is_added_to_the_end_of_its_section() {
        let before = "[FrameGen]\r\nEnabled=true\r\n\r\n[Other]\r\nKeep=1\r\n";
        let after = set(before, "FrameGen", &[("FGOutput", "fsrfg")]);
        assert_eq!(
            after,
            "[FrameGen]\r\nEnabled=true\r\nFGOutput=fsrfg\r\n\r\n[Other]\r\nKeep=1\r\n"
        );
    }

    #[test]
    fn a_missing_section_is_appended() {
        let before = "[Upscalers]\r\nDx12Upscaler=dlss\r\n";
        let after = set(before, "FrameGen", &[("Enabled", "true")]);
        assert_eq!(
            after,
            "[Upscalers]\r\nDx12Upscaler=dlss\r\n\r\n[FrameGen]\r\nEnabled=true\r\n"
        );
    }

    #[test]
    fn an_empty_file_becomes_just_the_section() {
        let after = set("", "FrameGen", &[("Enabled", "true")]);
        assert_eq!(after, "[FrameGen]\r\nEnabled=true\r\n");
    }

    #[test]
    fn line_endings_survive() {
        // Rewriting a CRLF file as LF is a whole-file diff for no reason, and
        // makes an install look far more invasive than it was.
        let crlf = set("[A]\r\nX=1\r\n", "A", &[("Y", "2")]);
        assert!(!crlf.contains('\n') || crlf.contains("\r\n"));
        assert_eq!(crlf.matches("\r\n").count(), 3);

        let lf = set("[A]\nX=1\n", "A", &[("Y", "2")]);
        assert!(!lf.contains('\r'), "{lf:?}");
    }

    #[test]
    fn a_duplicated_section_is_written_to_as_one_unit() {
        // The reference implementation's defect: it takes the last matching
        // header as the start and the first following header as the end, so
        // here the end (index 2) precedes the start (index 4) and the new key
        // is inserted into the *first* copy of the section. Malformed input,
        // and real files have it.
        let before = "[A]\nX=1\n[B]\nY=2\n[A]\nZ=3\n";
        let after = set(before, "A", &[("W", "4")]);

        // The key belongs to the copy being written to, which is the last one.
        let lines: Vec<&str> = after.lines().collect();
        let last_a = lines
            .iter()
            .rposition(|line| *line == "[A]")
            .expect("the second copy");
        assert!(
            lines[last_a..].contains(&"W=4"),
            "went into the wrong copy: {lines:?}"
        );
        // And nothing was dropped.
        assert!(lines.contains(&"X=1"));
        assert!(lines.contains(&"Y=2"));
        assert!(lines.contains(&"Z=3"));
    }

    #[test]
    fn matching_ignores_case_and_leaves_the_users_spelling_alone() {
        // INI readers are generally case-insensitive, so a file written with a
        // different capitalisation must not gain a second copy of the same key
        // - two `Enabled` lines and the reader picks one. Having matched it
        // there is nothing to gain by restyling their key, so only the value
        // changes.
        let after = set(
            "[FrameGen]\r\nenabled=false\r\n",
            "FrameGen",
            &[("Enabled", "true")],
        );
        assert_eq!(after, "[FrameGen]\r\nenabled=true\r\n");
        assert_eq!(after.to_lowercase().matches("enabled=").count(), 1);
    }

    #[test]
    fn the_sections_own_spacing_is_kept() {
        // A real OptiScaler.ini writes `Key = value` throughout. Rewriting
        // four of its sixteen hundred lines as `Key=value` is a change nobody
        // asked for, and it shows up in every diff of that file afterwards.
        let before = "[FrameGen]\r\nEnabled = auto\r\nFGInput = auto\r\n";
        let after = set(
            before,
            "FrameGen",
            &[("Enabled", "true"), ("FGOutput", "fsrfg")],
        );
        assert_eq!(
            after,
            "[FrameGen]\r\nEnabled = true\r\nFGInput = auto\r\nFGOutput = fsrfg\r\n"
        );
    }

    #[test]
    fn indentation_on_a_key_line_survives() {
        let after = set("[A]\n    Key=old\n", "A", &[("Key", "new")]);
        assert_eq!(after, "[A]\n    Key=new\n");
    }

    #[test]
    fn a_section_name_with_spaces_is_still_found() {
        let after = set(
            "[ FrameGen ]\r\nEnabled=false\r\n",
            "FrameGen",
            &[("Enabled", "true")],
        );
        assert!(after.contains("Enabled=true"), "{after}");
        // The header the user wrote is left as they wrote it.
        assert!(after.contains("[ FrameGen ]"), "{after}");
    }

    #[test]
    fn setting_the_same_values_twice_changes_nothing_the_second_time() {
        // Idempotence, because an install that runs twice must not accumulate
        // keys or blank lines.
        let values = [("Enabled", "true"), ("FGInput", "upscaler")];
        let once = set("[FrameGen]\r\n", "FrameGen", &values);
        let twice = set(&once, "FrameGen", &values);
        assert_eq!(once, twice);
    }

    #[test]
    fn reading_back_what_was_written() {
        let text = set("", "OptiFG", &[("HUDFix", "true")]);
        assert_eq!(get(&text, "OptiFG", "HUDFix").as_deref(), Some("true"));
        assert_eq!(get(&text, "OptiFG", "Missing"), None);
        assert_eq!(get(&text, "Missing", "HUDFix"), None);
        // Case-insensitive both ways, and whitespace-trimmed.
        let spaced = "[A]\n  Key  =  value  \n";
        assert_eq!(get(spaced, "a", "KEY").as_deref(), Some("value"));
    }

    #[test]
    fn a_commented_key_is_not_read_as_a_value() {
        assert_eq!(get("[A]\n;Key=value\n", "A", "Key"), None);
    }
}
