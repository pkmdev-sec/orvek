use super::{DiffSnapshot, PatchSide};

impl DiffSnapshot {
    pub(in crate::review) fn contains_anchor(
        &self,
        path: &str,
        side: PatchSide,
        start_line: u32,
        end_line: u32,
    ) -> bool {
        if start_line == 0 || end_line < start_line {
            return false;
        }

        let mut old_path = None;
        let mut new_path = None;
        for line in self.patch.lines() {
            if line.starts_with("diff --git ") {
                old_path = None;
                new_path = None;
                continue;
            }
            if let Some(value) = line.strip_prefix("--- ") {
                old_path = patch_path(value);
                continue;
            }
            if let Some(value) = line.strip_prefix("+++ ") {
                new_path = patch_path(value);
                continue;
            }
            let Some((old_start, old_count, new_start, new_count)) = parse_hunk_header(line) else {
                continue;
            };
            let (candidate_path, first, count) = match side {
                PatchSide::Additions => (new_path.as_deref(), new_start, new_count),
                PatchSide::Deletions => (old_path.as_deref(), old_start, old_count),
            };
            if candidate_path != Some(path) || count == 0 {
                continue;
            }
            let Some(last) = first.checked_add(count - 1) else {
                continue;
            };
            if start_line >= first && end_line <= last {
                return true;
            }
        }
        false
    }
}

fn patch_path(value: &str) -> Option<String> {
    let value = value.split('\t').next().unwrap_or(value);
    if value == "/dev/null" {
        return None;
    }
    let value = decode_git_path(value)?;
    Some(
        value
            .strip_prefix("a/")
            .or_else(|| value.strip_prefix("b/"))
            .unwrap_or(&value)
            .to_owned(),
    )
}

fn decode_git_path(value: &str) -> Option<String> {
    let Some(quoted) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Some(value.to_owned());
    };
    let bytes = quoted.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        index += 1;
        let escaped = *bytes.get(index)?;
        if escaped.is_ascii_digit() && escaped < b'8' {
            let mut value = 0_u8;
            let mut digits = 0;
            while digits < 3 {
                let Some(digit) = bytes.get(index).copied() else {
                    break;
                };
                if !(b'0'..=b'7').contains(&digit) {
                    break;
                }
                value = value.checked_mul(8)?.checked_add(digit - b'0')?;
                index += 1;
                digits += 1;
            }
            decoded.push(value);
            continue;
        }
        decoded.push(match escaped {
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 0x0b,
            b'\\' => b'\\',
            b'"' => b'"',
            _ => return None,
        });
        index += 1;
    }
    String::from_utf8(decoded).ok()
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let header = line.strip_prefix("@@ -")?;
    let (old, remainder) = header.split_once(" +")?;
    let (new, _) = remainder.split_once(" @@")?;
    let (old_start, old_count) = parse_hunk_range(old)?;
    let (new_start, new_count) = parse_hunk_range(new)?;
    Some((old_start, old_count, new_start, new_count))
}

fn parse_hunk_range(value: &str) -> Option<(u32, u32)> {
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::diff::{OverviewContext, OverviewRange};
    use orvek_harness::Digest;
    fn snapshot(patch: &str) -> DiffSnapshot {
        DiffSnapshot {
            patch: patch.into(),
            overview: OverviewContext {
                repository: "/fixture".into(),
                range: OverviewRange::WorkingTree {
                    base: "fixture".into(),
                },
                manifest: Digest::of(b"manifest"),
            },
            repository: "fixture".into(),
            scope: "Frozen range".into(),
            base: "fixture".into(),
            manifest: Digest::of(b"manifest"),
            source_identity: Digest::of(b"source"),
            metadata_changes: Vec::new(),
        }
    }
    #[test]
    fn quoted_unicode_paths_and_side_extents_remain_exact() {
        let view = snapshot(
            "diff --git \"a/caf\\303\\251.rs\" \"b/caf\\303\\251.rs\"\n--- \"a/caf\\303\\251.rs\"\n+++ \"b/caf\\303\\251.rs\"\n@@ -8,1 +8,2 @@\n-old\n+new\n+extra\n",
        );
        assert!(view.contains_anchor("café.rs", PatchSide::Additions, 8, 9));
        assert!(view.contains_anchor("café.rs", PatchSide::Deletions, 8, 8));
        assert!(!view.contains_anchor("café.rs", PatchSide::Deletions, 8, 9));
        assert!(!view.contains_anchor("other", PatchSide::Additions, 8, 8));
        assert!(!view.contains_anchor("café.rs", PatchSide::Additions, 0, 8));
    }
    #[test]
    fn malformed_hunks_and_path_escapes_never_create_anchors() {
        assert_eq!(decode_git_path("\"bad\\q\""), None);
        assert_eq!(decode_git_path("\"bad\\777\""), None);
        assert_eq!(parse_hunk_header("@@ -not-a-number +1 @@"), None);
        let view =
            snapshot("diff --git a/new b/new\n--- /dev/null\n+++ b/new\n@@ -0,0 +1 @@\n+new\n");
        assert!(!view.contains_anchor("new", PatchSide::Deletions, 1, 1));
        assert!(view.contains_anchor("new", PatchSide::Additions, 1, 1));
    }
}
