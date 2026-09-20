use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Analyzer {
    Rust,
    Generic,
}

#[derive(Clone, Debug)]
pub(super) struct Language {
    name: String,
    analyzer: Analyzer,
    line_comments: &'static [&'static str],
    block_comments: &'static [(&'static str, &'static str)],
}

impl Language {
    fn new(
        name: impl Into<String>,
        analyzer: Analyzer,
        line_comments: &'static [&'static str],
        block_comments: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            name: name.into(),
            analyzer,
            line_comments,
            block_comments,
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) const fn analyzer(&self) -> Analyzer {
        self.analyzer
    }

    pub(super) fn strip_comments(&self, source: &str) -> String {
        if self.analyzer == Analyzer::Rust {
            return super::strip_rust_comments(source);
        }
        strip_comments(source, self.line_comments, self.block_comments)
    }
}

#[derive(Clone, Copy)]
struct LanguageProfile {
    name: &'static str,
    extensions: &'static [&'static str],
    analyzer: Analyzer,
    line_comments: &'static [&'static str],
    block_comments: &'static [(&'static str, &'static str)],
}

impl LanguageProfile {
    const fn new(
        name: &'static str,
        extensions: &'static [&'static str],
        analyzer: Analyzer,
        line_comments: &'static [&'static str],
        block_comments: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            name,
            extensions,
            analyzer,
            line_comments,
            block_comments,
        }
    }

    fn language(self) -> Language {
        Language::new(
            self.name,
            self.analyzer,
            self.line_comments,
            self.block_comments,
        )
    }
}

const NONE: &[&str] = &[];
const NO_BLOCKS: &[(&str, &str)] = &[];
const HASH: &[&str] = &["#"];
const SLASH: &[&str] = &["//"];
const HASH_SLASH: &[&str] = &["#", "//"];
const DASH: &[&str] = &["--"];
const SEMICOLON: &[&str] = &[";"];
const PERCENT: &[&str] = &["%"];
const BANG: &[&str] = &["!"];
const QUOTE: &[&str] = &["'"];
const COBOL: &[&str] = &["*>"];
const C_BLOCK: &[(&str, &str)] = &[("/*", "*/")];
const HTML_BLOCK: &[(&str, &str)] = &[("<!--", "-->")];
const ML_BLOCK: &[(&str, &str)] = &[("(*", "*)")];
const HASKELL_BLOCK: &[(&str, &str)] = &[("{-", "-}")];
const LUA_BLOCK: &[(&str, &str)] = &[("--[[", "]]")];
const JULIA_BLOCK: &[(&str, &str)] = &[("#=", "=#")];
const BATCH: &[&str] = &["rem ", "::"];
const PERCENT_SLASH: &[&str] = &["%", "//"];

const LANGUAGE_PROFILES: &[LanguageProfile] = &[
    LanguageProfile::new("rust", &["rs"], Analyzer::Rust, SLASH, C_BLOCK),
    LanguageProfile::new(
        "python",
        &["py", "pyi", "pyw"],
        Analyzer::Generic,
        HASH,
        NO_BLOCKS,
    ),
    LanguageProfile::new(
        "javascript",
        &["js", "mjs", "cjs", "jsx"],
        Analyzer::Generic,
        SLASH,
        C_BLOCK,
    ),
    LanguageProfile::new(
        "typescript",
        &["ts", "mts", "cts", "tsx"],
        Analyzer::Generic,
        SLASH,
        C_BLOCK,
    ),
    LanguageProfile::new("go", &["go"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new("c", &["c", "h"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new(
        "c++",
        &["cc", "cpp", "cxx", "hh", "hpp", "hxx"],
        Analyzer::Generic,
        SLASH,
        C_BLOCK,
    ),
    LanguageProfile::new("c#", &["cs"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new("java", &["java"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new("kotlin", &["kt", "kts"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new("swift", &["swift"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new("scala", &["scala", "sc"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new("dart", &["dart"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new(
        "groovy",
        &["groovy", "gradle"],
        Analyzer::Generic,
        SLASH,
        C_BLOCK,
    ),
    LanguageProfile::new(
        "php",
        &["php", "phtml"],
        Analyzer::Generic,
        HASH_SLASH,
        C_BLOCK,
    ),
    LanguageProfile::new(
        "ruby",
        &["rb", "rake", "gemspec"],
        Analyzer::Generic,
        HASH,
        NO_BLOCKS,
    ),
    LanguageProfile::new(
        "shell",
        &["sh", "bash", "zsh", "fish"],
        Analyzer::Generic,
        HASH,
        NO_BLOCKS,
    ),
    LanguageProfile::new(
        "perl",
        &["pl", "pm", "t"],
        Analyzer::Generic,
        HASH,
        NO_BLOCKS,
    ),
    LanguageProfile::new("r", &["r"], Analyzer::Generic, HASH, NO_BLOCKS),
    LanguageProfile::new("julia", &["jl"], Analyzer::Generic, HASH, JULIA_BLOCK),
    LanguageProfile::new("lua", &["lua"], Analyzer::Generic, DASH, LUA_BLOCK),
    LanguageProfile::new("sql", &["sql"], Analyzer::Generic, DASH, C_BLOCK),
    LanguageProfile::new(
        "haskell",
        &["hs", "lhs"],
        Analyzer::Generic,
        DASH,
        HASKELL_BLOCK,
    ),
    LanguageProfile::new(
        "erlang",
        &["erl", "hrl"],
        Analyzer::Generic,
        PERCENT,
        NO_BLOCKS,
    ),
    LanguageProfile::new("elixir", &["ex", "exs"], Analyzer::Generic, HASH, NO_BLOCKS),
    LanguageProfile::new(
        "clojure",
        &["clj", "cljs", "cljc", "edn"],
        Analyzer::Generic,
        SEMICOLON,
        NO_BLOCKS,
    ),
    LanguageProfile::new(
        "lisp",
        &["lisp", "lsp", "el", "scm", "rkt"],
        Analyzer::Generic,
        SEMICOLON,
        NO_BLOCKS,
    ),
    LanguageProfile::new("ocaml", &["ml", "mli"], Analyzer::Generic, NONE, ML_BLOCK),
    LanguageProfile::new(
        "f#",
        &["fs", "fsx", "fsi"],
        Analyzer::Generic,
        SLASH,
        ML_BLOCK,
    ),
    LanguageProfile::new(
        "visual-basic",
        &["vb", "vbs"],
        Analyzer::Generic,
        QUOTE,
        NO_BLOCKS,
    ),
    LanguageProfile::new(
        "fortran",
        &["f", "f77", "f90", "f95", "f03", "f08"],
        Analyzer::Generic,
        BANG,
        NO_BLOCKS,
    ),
    LanguageProfile::new("ada", &["adb", "ads"], Analyzer::Generic, DASH, NO_BLOCKS),
    LanguageProfile::new(
        "objective-c-or-matlab",
        &["m"],
        Analyzer::Generic,
        PERCENT_SLASH,
        C_BLOCK,
    ),
    LanguageProfile::new("objective-c++", &["mm"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new("solidity", &["sol"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new("zig", &["zig"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new("nim", &["nim"], Analyzer::Generic, HASH, NO_BLOCKS),
    LanguageProfile::new("crystal", &["cr"], Analyzer::Generic, HASH, NO_BLOCKS),
    LanguageProfile::new(
        "assembly",
        &["asm", "s", "inc"],
        Analyzer::Generic,
        SEMICOLON,
        NO_BLOCKS,
    ),
    LanguageProfile::new(
        "cobol",
        &["cob", "cbl"],
        Analyzer::Generic,
        COBOL,
        NO_BLOCKS,
    ),
    LanguageProfile::new("pascal", &["pas", "pp"], Analyzer::Generic, SLASH, ML_BLOCK),
    LanguageProfile::new("protobuf", &["proto"], Analyzer::Generic, SLASH, C_BLOCK),
    LanguageProfile::new(
        "graphql",
        &["graphql", "gql"],
        Analyzer::Generic,
        HASH,
        NO_BLOCKS,
    ),
    LanguageProfile::new(
        "markup",
        &["html", "htm", "xml", "svg"],
        Analyzer::Generic,
        NONE,
        HTML_BLOCK,
    ),
    LanguageProfile::new(
        "component-markup",
        &["vue", "svelte"],
        Analyzer::Generic,
        SLASH,
        HTML_BLOCK,
    ),
    LanguageProfile::new(
        "stylesheet",
        &["css", "scss", "sass", "less"],
        Analyzer::Generic,
        SLASH,
        C_BLOCK,
    ),
    LanguageProfile::new(
        "json",
        &["json", "jsonc"],
        Analyzer::Generic,
        SLASH,
        C_BLOCK,
    ),
    LanguageProfile::new("toml", &["toml"], Analyzer::Generic, HASH, NO_BLOCKS),
    LanguageProfile::new("yaml", &["yaml", "yml"], Analyzer::Generic, HASH, NO_BLOCKS),
    LanguageProfile::new(
        "hcl",
        &["tf", "tfvars", "hcl"],
        Analyzer::Generic,
        HASH_SLASH,
        C_BLOCK,
    ),
    LanguageProfile::new("nix", &["nix"], Analyzer::Generic, HASH, C_BLOCK),
    LanguageProfile::new("cmake", &["cmake"], Analyzer::Generic, HASH, NO_BLOCKS),
    LanguageProfile::new("make", &["mk"], Analyzer::Generic, HASH, NO_BLOCKS),
    LanguageProfile::new(
        "powershell",
        &["ps1", "psm1"],
        Analyzer::Generic,
        HASH,
        C_BLOCK,
    ),
    LanguageProfile::new(
        "batch",
        &["bat", "cmd"],
        Analyzer::Generic,
        BATCH,
        NO_BLOCKS,
    ),
    LanguageProfile::new(
        "tex",
        &["tex", "sty", "cls"],
        Analyzer::Generic,
        PERCENT,
        NO_BLOCKS,
    ),
];

const IGNORED_FILE_NAMES: &[&str] = &[
    ".ds_store",
    "cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "poetry.lock",
    "composer.lock",
    "go.sum",
    "license",
    "license.md",
    "license.txt",
    "notice",
    "notice.md",
    "readme",
    "readme.md",
    "changelog",
    "changelog.md",
];

const IGNORED_EXTENSIONS: &[&str] = &[
    "md", "mdx", "rst", "txt", "lock", "sum", "log", "csv", "tsv", "pdf", "doc", "docx", "rtf",
    "png", "jpg", "jpeg", "gif", "webp", "ico", "bmp", "tiff", "mp3", "mp4", "mov", "avi", "wav",
    "flac", "zip", "gz", "bz2", "xz", "7z", "tar", "dmg", "iso", "woff", "woff2", "ttf", "otf",
    "db", "sqlite", "sqlite3", "class", "jar", "o", "a", "so", "dylib", "dll", "exe", "bin",
    "wasm", "map", "snap",
];

pub(super) fn could_be_source(path: &Path) -> bool {
    let Some(file_name) = path.file_name() else {
        return false;
    };
    let lower_name = file_name.to_string_lossy().to_ascii_lowercase();
    if ignored_file_name(&lower_name) {
        return false;
    }
    if language_for_file_name(&lower_name).is_some() {
        return true;
    }
    path.extension()
        .and_then(|value| value.to_str())
        .is_none_or(|extension| !ignored_extension(&extension.to_ascii_lowercase()))
}

/// Identify source without requiring a finite registry. Known languages get
/// their conventional comment syntax; an unfamiliar extension still receives
/// deterministic generic analysis instead of being silently ignored.
pub(super) fn detect(path: &Path, source: &str) -> Option<Language> {
    if !looks_textual(source) {
        return None;
    }

    let file_name = path.file_name()?.to_string_lossy();
    let lower_name = file_name.to_ascii_lowercase();
    if ignored_file_name(&lower_name) {
        return None;
    }
    if let Some(language) = language_for_file_name(&lower_name) {
        return Some(language);
    }

    let extension = path.extension().and_then(|value| value.to_str());
    let Some(extension) = extension else {
        return language_for_shebang(source);
    };
    let extension = extension.to_ascii_lowercase();
    if ignored_extension(&extension) {
        return None;
    }
    Some(
        language_for_extension(&extension)
            .unwrap_or_else(|| Language::new(extension, Analyzer::Generic, NONE, NO_BLOCKS)),
    )
}

fn language_for_file_name(name: &str) -> Option<Language> {
    let language = match name {
        "dockerfile" | "containerfile" => generic("dockerfile", HASH, NO_BLOCKS),
        "makefile" | "gnumakefile" => generic("make", HASH, NO_BLOCKS),
        "cmakelists.txt" => generic("cmake", HASH, NO_BLOCKS),
        "rakefile" | "gemfile" | "podfile" | "vagrantfile" => generic("ruby", HASH, NO_BLOCKS),
        "jenkinsfile" => generic("groovy", SLASH, C_BLOCK),
        "justfile" => generic("just", HASH, NO_BLOCKS),
        "build" | "workspace" => generic("starlark", HASH, NO_BLOCKS),
        _ => return None,
    };
    Some(language)
}

fn language_for_extension(extension: &str) -> Option<Language> {
    LANGUAGE_PROFILES
        .iter()
        .find(|profile| profile.extensions.contains(&extension))
        .copied()
        .map(LanguageProfile::language)
}

fn language_for_shebang(source: &str) -> Option<Language> {
    let first = source
        .lines()
        .next()?
        .strip_prefix("#!")?
        .to_ascii_lowercase();
    let (name, comments) = if first.contains("python") {
        ("python", HASH)
    } else if first.contains("ruby") {
        ("ruby", HASH)
    } else if first.contains("perl") {
        ("perl", HASH)
    } else if first.contains("node") || first.contains("deno") {
        ("javascript", SLASH)
    } else if first.contains("php") {
        ("php", HASH_SLASH)
    } else if first.contains("sh") || first.contains("fish") {
        ("shell", HASH)
    } else {
        ("script", HASH)
    };
    Some(generic(name, comments, NO_BLOCKS))
}

fn generic(
    name: &'static str,
    line_comments: &'static [&'static str],
    block_comments: &'static [(&'static str, &'static str)],
) -> Language {
    Language::new(name, Analyzer::Generic, line_comments, block_comments)
}

fn ignored_file_name(name: &str) -> bool {
    IGNORED_FILE_NAMES.contains(&name)
}

fn ignored_extension(extension: &str) -> bool {
    IGNORED_EXTENSIONS.contains(&extension)
}

fn looks_textual(source: &str) -> bool {
    !source.as_bytes().contains(&0)
        && source
            .chars()
            .filter(|character| character.is_control() && !character.is_whitespace())
            .take(2)
            .count()
            < 2
}

fn strip_comments(source: &str, line_comments: &[&str], block_comments: &[(&str, &str)]) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut quote = None;
    let mut block_end: Option<&str> = None;

    while index < bytes.len() {
        if let Some(end) = block_end {
            if bytes[index..].starts_with(end.as_bytes()) {
                output.extend(std::iter::repeat_n(b' ', end.len()));
                index += end.len();
                block_end = None;
            } else {
                output.push(if bytes[index] == b'\n' { b'\n' } else { b' ' });
                index += 1;
            }
            continue;
        }

        if let Some(delimiter) = quote {
            output.push(bytes[index]);
            if bytes[index] == b'\\' && index + 1 < bytes.len() {
                output.push(bytes[index + 1]);
                index += 2;
            } else {
                if bytes[index] == delimiter {
                    quote = None;
                }
                index += 1;
            }
            continue;
        }

        // Test blocks first because several languages use a line-comment
        // prefix at the start of their block marker (`#=` and `--[[`).
        if let Some((start, end)) = block_comments
            .iter()
            .find(|(start, _)| bytes[index..].starts_with(start.as_bytes()))
        {
            output.extend(std::iter::repeat_n(b' ', start.len()));
            index += start.len();
            block_end = Some(*end);
            continue;
        }
        if let Some(marker) = line_comments
            .iter()
            .find(|marker| bytes[index..].starts_with(marker.as_bytes()))
        {
            output.extend(std::iter::repeat_n(b' ', marker.len()));
            index += marker.len();
            while index < bytes.len() && bytes[index] != b'\n' {
                output.push(b' ');
                index += 1;
            }
            continue;
        }
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            quote = Some(bytes[index]);
        }
        output.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(output).expect("comment removal preserves UTF-8 bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_extensions_receive_generic_analysis() {
        let language = detect(Path::new("example.futurelang"), "construct value\n").unwrap();
        assert_eq!(language.name(), "futurelang");
        assert_eq!(language.analyzer(), Analyzer::Generic);
    }

    #[test]
    fn extensionless_scripts_use_their_shebang() {
        let language = detect(Path::new("tool"), "#!/usr/bin/env python3\nprint('ok')\n").unwrap();
        assert_eq!(language.name(), "python");
    }

    #[test]
    fn generic_comment_removal_preserves_strings_and_lines() {
        let language = detect(Path::new("tool.py"), "print('# text') # comment\n# line\n").unwrap();
        let cleaned = language.strip_comments("print('# text') # comment\n# line\n");
        assert!(cleaned.lines().next().unwrap().contains("# text"));
        assert_eq!(cleaned.lines().count(), 2);
        assert!(!cleaned.contains("comment"));
    }
}
