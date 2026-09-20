//! Deterministic, host-owned signals for code growth, duplication, and complexity.
//!
//! These metrics are diagnostics rather than a scalar quality score. Callers can
//! compare reports, but should not turn any one signal into an optimization target.

mod language;

use language::Analyzer;
use proc_macro2::Span;
use rayon::prelude::*;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};
use syn::{
    BinOp, Expr, ExprBlock, ExprLit, ImplItemFn, ItemFn, Lit, Pat, Stmt, TraitItemFn,
    spanned::Spanned,
    visit::{self, Visit},
};
use thiserror::Error;

pub const COMPLEXITY_THRESHOLD: u32 = 10;

#[derive(Clone, Debug)]
pub struct AnalysisPolicy {
    pub clone_window: usize,
    pub min_clone_bytes: usize,
    pub max_hotspots: usize,
}

impl Default for AnalysisPolicy {
    fn default() -> Self {
        Self {
            clone_window: 4,
            min_clone_bytes: 80,
            max_hotspots: 20,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SloppinessReport {
    pub version: u32,
    /// `mixed` when more than one source language was found.
    pub language: String,
    /// Deterministic file counts keyed by detected language.
    pub languages: BTreeMap<String, usize>,
    pub files: usize,
    pub bytes: u64,
    pub source_lines: usize,
    pub verbosity: VerbosityReport,
    pub erosion: ErosionReport,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerbosityReport {
    pub ast_flagged_lines: usize,
    pub clone_lines: usize,
    pub flagged_or_clone_lines: usize,
    pub clone_groups: usize,
    pub ratio: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErosionReport {
    pub complexity_threshold: u32,
    pub functions: usize,
    pub complex_functions: usize,
    pub total_mass: f64,
    pub complex_mass: f64,
    pub ratio: f64,
    pub hotspots: Vec<FunctionMetric>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FunctionMetric {
    pub path: String,
    pub name: String,
    pub start_line: usize,
    pub source_lines: u32,
    pub cyclomatic_complexity: u32,
    pub mass: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SloppinessAssessment {
    pub current: SloppinessReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<SloppinessReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<SloppinessDelta>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SloppinessDelta {
    pub source_lines: i64,
    pub verbosity_ratio: f64,
    pub erosion_ratio: f64,
}

#[derive(Debug, Error)]
pub enum SloppinessError {
    #[error("sloppiness analysis I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sloppiness analysis root is not a file or directory: {0}")]
    InvalidRoot(PathBuf),
    #[error("sloppiness analysis policy is invalid")]
    InvalidPolicy,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct LineId {
    path: String,
    line: usize,
}

struct SourceFile {
    path: String,
    language: language::Language,
    bytes: u64,
    lines: Vec<Option<String>>,
}

struct FileAnalysis {
    file: Option<SourceFile>,
    ast_lines: BTreeSet<LineId>,
    functions: Vec<FunctionMetric>,
    limitation: Option<String>,
}

/// Analyze source under `root` without invoking a model, shell, network,
/// compiler, or external quality service. Unknown textual extensions receive
/// generic line and clone analysis rather than being excluded.
pub fn analyze(root: &Path) -> Result<SloppinessReport, SloppinessError> {
    analyze_with_policy(root, &AnalysisPolicy::default())
}

pub fn assess(
    current: &Path,
    baseline: Option<&Path>,
) -> Result<SloppinessAssessment, SloppinessError> {
    let current = analyze(current)?;
    let baseline = baseline.map(analyze).transpose()?;
    let delta = baseline
        .as_ref()
        .map(|baseline| compare(baseline, &current));
    Ok(SloppinessAssessment {
        current,
        baseline,
        delta,
    })
}

pub fn compare(baseline: &SloppinessReport, current: &SloppinessReport) -> SloppinessDelta {
    SloppinessDelta {
        source_lines: current.source_lines as i64 - baseline.source_lines as i64,
        verbosity_ratio: current.verbosity.ratio - baseline.verbosity.ratio,
        erosion_ratio: current.erosion.ratio - baseline.erosion.ratio,
    }
}

pub fn analyze_with_policy(
    root: &Path,
    policy: &AnalysisPolicy,
) -> Result<SloppinessReport, SloppinessError> {
    if policy.clone_window < 2 || policy.min_clone_bytes == 0 || policy.max_hotspots == 0 {
        return Err(SloppinessError::InvalidPolicy);
    }

    let mut paths = Vec::new();
    collect_source_files(root, root, &mut paths)?;
    paths.sort();

    let batch_size = analysis_batch_size(paths.len(), rayon::current_num_threads());
    let analyzed = paths
        .par_chunks(batch_size)
        .map(|batch| {
            batch
                .iter()
                .map(|path| analyze_file(root, path))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    let mut files = Vec::with_capacity(paths.len());
    let mut ast_lines = BTreeSet::new();
    let mut functions = Vec::new();
    let mut limitations = Vec::new();
    let mut omitted_limitations = 0usize;
    for result in analyzed {
        let analysis = result?;
        ast_lines.extend(analysis.ast_lines);
        functions.extend(analysis.functions);
        if let Some(limitation) = analysis.limitation {
            push_limitation(&mut limitations, &mut omitted_limitations, limitation);
        }
        if let Some(file) = analysis.file {
            files.push(file);
        }
    }

    let bytes = files.iter().map(|file| file.bytes).sum();
    let source_lines = files
        .iter()
        .map(|file| file.lines.iter().filter(|line| line.is_some()).count())
        .sum();
    let generic_files = files
        .iter()
        .filter(|file| file.language.analyzer() == Analyzer::Generic)
        .count();
    if generic_files > 0 {
        push_limitation(
            &mut limitations,
            &mut omitted_limitations,
            format!(
                "{generic_files} non-Rust source files contribute to line and clone metrics; \
                 redundant-AST and complexity metrics currently require a language adapter"
            ),
        );
    }
    if omitted_limitations > 0 {
        limitations.push(format!(
            "{omitted_limitations} additional limitations omitted"
        ));
    }

    let (clone_lines, clone_groups) = clone_lines(&files, policy);
    let flagged_or_clone_lines = ast_lines.union(&clone_lines).count();
    let verbosity = VerbosityReport {
        ast_flagged_lines: ast_lines.len(),
        clone_lines: clone_lines.len(),
        flagged_or_clone_lines,
        clone_groups,
        ratio: ratio(flagged_or_clone_lines as f64, source_lines as f64),
    };
    let erosion = erosion_report(functions, policy.max_hotspots);
    let mut languages = BTreeMap::new();
    for file in &files {
        *languages
            .entry(file.language.name().to_owned())
            .or_insert(0) += 1;
    }
    let language = match languages.len() {
        0 => "unknown".to_owned(),
        1 => languages.keys().next().cloned().unwrap_or_default(),
        _ => "mixed".to_owned(),
    };

    Ok(SloppinessReport {
        version: 2,
        language,
        languages,
        files: files.len(),
        bytes,
        source_lines,
        verbosity,
        erosion,
        limitations,
    })
}

fn analysis_batch_size(files: usize, workers: usize) -> usize {
    files.div_ceil(workers.max(1).saturating_mul(4)).max(1)
}

fn analyze_file(root: &Path, path: &Path) -> Result<FileAnalysis, SloppinessError> {
    let data = fs::read(path).map_err(|source| SloppinessError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let relative = relative_name(root, path);
    let source = match String::from_utf8(data) {
        Ok(source) => source,
        Err(_) => {
            return Ok(FileAnalysis {
                file: None,
                ast_lines: BTreeSet::new(),
                functions: Vec::new(),
                limitation: Some(format!("{relative} is not UTF-8 and was not analyzed")),
            });
        }
    };
    let Some(language) = language::detect(path, &source) else {
        return Ok(FileAnalysis {
            file: None,
            ast_lines: BTreeSet::new(),
            functions: Vec::new(),
            limitation: None,
        });
    };
    let bytes = source.len() as u64;
    let cleaned = language.strip_comments(&source);
    let lines = cleaned
        .lines()
        .map(normalize_line)
        .map(|line| (!line.is_empty()).then_some(line))
        .collect::<Vec<_>>();
    let mut ast_lines = BTreeSet::new();
    let mut functions = Vec::new();
    let limitation = if language.analyzer() == Analyzer::Rust {
        match syn::parse_file(&source) {
            Ok(syntax) => {
                let code_lines = lines.iter().map(Option::is_some).collect::<Vec<_>>();
                let mut collector = FunctionCollector {
                    path: &relative,
                    code_lines: &code_lines,
                    functions: &mut functions,
                };
                collector.visit_file(&syntax);
                let mut redundancy = RedundancyVisitor {
                    path: &relative,
                    code_lines: &code_lines,
                    lines: &mut ast_lines,
                };
                redundancy.visit_file(&syntax);
                None
            }
            Err(_) => Some(format!(
                "{relative} could not be parsed; its lines contribute only to verbosity"
            )),
        }
    } else {
        None
    };
    Ok(FileAnalysis {
        file: Some(SourceFile {
            path: relative,
            language,
            bytes,
            lines,
        }),
        ast_lines,
        functions,
        limitation,
    })
}

fn push_limitation(limitations: &mut Vec<String>, omitted: &mut usize, limitation: String) {
    if limitations.len() < 20 {
        limitations.push(limitation);
    } else {
        *omitted += 1;
    }
}

fn collect_source_files(
    root: &Path,
    path: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), SloppinessError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| SloppinessError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        if language::could_be_source(path) {
            output.push(path.to_path_buf());
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return if path == root {
            Err(SloppinessError::InvalidRoot(path.to_path_buf()))
        } else {
            Ok(())
        };
    }

    let entries = fs::read_dir(path).map_err(|source| SloppinessError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SloppinessError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let child = entry.path();
        if child != root && child.is_dir() && excluded_directory(&child) {
            continue;
        }
        collect_source_files(root, &child, output)?;
    }
    Ok(())
}

fn excluded_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                ".git"
                    | ".orvek"
                    | ".Trash"
                    | ".venv"
                    | "build"
                    | "coverage"
                    | "dist"
                    | "node_modules"
                    | "target"
                    | "vendor"
            )
        })
}

fn relative_name(root: &Path, path: &Path) -> String {
    let base = if root.is_file() {
        root.parent().unwrap_or_else(|| Path::new(""))
    } else {
        root
    };
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn normalize_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator == 0.0 {
        0.0
    } else {
        numerator / denominator
    }
}

fn clone_lines(files: &[SourceFile], policy: &AnalysisPolicy) -> (BTreeSet<LineId>, usize) {
    type Digest = [u8; 32];
    struct Occurrence {
        file: usize,
        lines: Box<[usize]>,
    }

    let mut windows = BTreeMap::<Digest, Vec<Occurrence>>::new();
    for (file_index, file) in files.iter().enumerate() {
        let meaningful = file
            .lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| line.as_ref().map(|line| (index + 1, line)))
            .collect::<Vec<_>>();
        for window in meaningful.windows(policy.clone_window) {
            let content_bytes = window.iter().map(|(_, line)| line.len()).sum::<usize>();
            if content_bytes < policy.min_clone_bytes {
                continue;
            }
            let mut hasher = Sha256::new();
            for (_, line) in window {
                hasher.update(line.as_bytes());
                hasher.update([0]);
            }
            windows
                .entry(hasher.finalize().into())
                .or_default()
                .push(Occurrence {
                    file: file_index,
                    lines: window
                        .iter()
                        .map(|(line, _)| *line)
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                });
        }
    }

    let mut lines = BTreeSet::new();
    let mut groups = 0;
    for occurrences in windows.values() {
        let duplicated = occurrences
            .iter()
            .enumerate()
            .filter(|(index, occurrence)| {
                occurrences.iter().enumerate().any(|(other_index, other)| {
                    *index != other_index
                        && (occurrence.file != other.file
                            || occurrence
                                .lines
                                .iter()
                                .all(|line| !other.lines.contains(line)))
                })
            })
            .map(|(_, occurrence)| occurrence)
            .collect::<Vec<_>>();
        if duplicated.len() >= 2 {
            groups += 1;
            for occurrence in duplicated {
                lines.extend(occurrence.lines.iter().map(|line| LineId {
                    path: files[occurrence.file].path.clone(),
                    line: *line,
                }));
            }
        }
    }
    (lines, groups)
}

fn erosion_report(mut functions: Vec<FunctionMetric>, max_hotspots: usize) -> ErosionReport {
    let function_count = functions.len();
    let total_mass = functions.iter().map(|function| function.mass).sum::<f64>();
    let complex_mass = functions
        .iter()
        .filter(|function| function.cyclomatic_complexity > COMPLEXITY_THRESHOLD)
        .map(|function| function.mass)
        .sum::<f64>();
    let complex_functions = functions
        .iter()
        .filter(|function| function.cyclomatic_complexity > COMPLEXITY_THRESHOLD)
        .count();
    functions.sort_by(|left, right| {
        right
            .mass
            .total_cmp(&left.mass)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.start_line.cmp(&right.start_line))
    });
    functions.truncate(max_hotspots);
    ErosionReport {
        complexity_threshold: COMPLEXITY_THRESHOLD,
        functions: function_count,
        complex_functions,
        total_mass,
        complex_mass,
        ratio: ratio(complex_mass, total_mass),
        hotspots: functions,
    }
}

struct FunctionCollector<'a> {
    path: &'a str,
    code_lines: &'a [bool],
    functions: &'a mut Vec<FunctionMetric>,
}

impl FunctionCollector<'_> {
    fn record<T: VisitWithComplexity>(&mut self, name: String, node: &T) {
        let span = node.node_span();
        let source_lines = count_lines(self.code_lines, span) as u32;
        let mut complexity = ComplexityVisitor { count: 1 };
        node.visit_complexity(&mut complexity);
        self.functions.push(FunctionMetric {
            path: self.path.to_owned(),
            name,
            start_line: span.start().line,
            source_lines,
            cyclomatic_complexity: complexity.count,
            mass: complexity.count as f64 * (source_lines as f64).sqrt(),
        });
    }
}

trait VisitWithComplexity {
    fn node_span(&self) -> Span;
    fn visit_complexity(&self, visitor: &mut ComplexityVisitor);
}

impl VisitWithComplexity for ItemFn {
    fn node_span(&self) -> Span {
        self.span()
    }

    fn visit_complexity(&self, visitor: &mut ComplexityVisitor) {
        visitor.visit_block(&self.block);
    }
}

impl VisitWithComplexity for ImplItemFn {
    fn node_span(&self) -> Span {
        self.span()
    }

    fn visit_complexity(&self, visitor: &mut ComplexityVisitor) {
        visitor.visit_block(&self.block);
    }
}

impl VisitWithComplexity for TraitItemFn {
    fn node_span(&self) -> Span {
        self.span()
    }

    fn visit_complexity(&self, visitor: &mut ComplexityVisitor) {
        if let Some(block) = &self.default {
            visitor.visit_block(block);
        }
    }
}

impl<'ast> Visit<'ast> for FunctionCollector<'_> {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.record(node.sig.ident.to_string(), node);
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.record(node.sig.ident.to_string(), node);
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast TraitItemFn) {
        if node.default.is_some() {
            self.record(node.sig.ident.to_string(), node);
        }
        visit::visit_trait_item_fn(self, node);
    }

    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        let span = node.span();
        let source_lines = count_lines(self.code_lines, span) as u32;
        let mut complexity = ComplexityVisitor { count: 1 };
        complexity.visit_expr(&node.body);
        self.functions.push(FunctionMetric {
            path: self.path.to_owned(),
            name: format!("<closure@{}>", span.start().line),
            start_line: span.start().line,
            source_lines,
            cyclomatic_complexity: complexity.count,
            mass: complexity.count as f64 * (source_lines as f64).sqrt(),
        });
        visit::visit_expr_closure(self, node);
    }
}

struct ComplexityVisitor {
    count: u32,
}

impl<'ast> Visit<'ast> for ComplexityVisitor {
    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        self.count = self.count.saturating_add(1);
        visit::visit_expr_if(self, node);
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.count = self.count.saturating_add(1);
        visit::visit_expr_for_loop(self, node);
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.count = self.count.saturating_add(1);
        visit::visit_expr_loop(self, node);
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.count = self.count.saturating_add(1);
        visit::visit_expr_while(self, node);
    }

    fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
        self.count = self
            .count
            .saturating_add(node.arms.len().saturating_sub(1) as u32);
        visit::visit_expr_match(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        if matches!(node.op, BinOp::And(_) | BinOp::Or(_)) {
            self.count = self.count.saturating_add(1);
        }
        visit::visit_expr_binary(self, node);
    }

    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        self.count = self.count.saturating_add(1);
        visit::visit_expr_try(self, node);
    }

    fn visit_expr_closure(&mut self, _node: &'ast syn::ExprClosure) {}

    fn visit_item_fn(&mut self, _node: &'ast ItemFn) {}
}

struct RedundancyVisitor<'a> {
    path: &'a str,
    code_lines: &'a [bool],
    lines: &'a mut BTreeSet<LineId>,
}

impl RedundancyVisitor<'_> {
    fn mark(&mut self, span: Span) {
        let start = span.start().line.max(1);
        let end = span.end().line.max(start).min(self.code_lines.len());
        for line in start..=end {
            if self.code_lines.get(line - 1).copied().unwrap_or(false) {
                self.lines.insert(LineId {
                    path: self.path.to_owned(),
                    line,
                });
            }
        }
    }
}

impl<'ast> Visit<'ast> for RedundancyVisitor<'_> {
    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        let then_value = bool_block(&node.then_branch);
        let else_value = node
            .else_branch
            .as_ref()
            .and_then(|(_, expression)| bool_expr(expression));
        if then_value.is_some() && else_value.is_some() {
            self.mark(node.span());
        }
        visit::visit_expr_if(self, node);
    }

    fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
        let values = node
            .arms
            .iter()
            .map(|arm| bool_expr(&arm.body))
            .collect::<Option<Vec<_>>>();
        let boolean_patterns = node.arms.iter().all(
            |arm| matches!(&arm.pat, Pat::Lit(literal) if matches!(literal.lit, Lit::Bool(_))),
        );
        let constant = values
            .as_ref()
            .is_some_and(|values| values.windows(2).all(|pair| pair[0] == pair[1]));
        if node.arms.len() >= 2
            && node.arms.iter().all(|arm| arm.guard.is_none())
            && values.is_some()
            && (boolean_patterns || constant)
        {
            self.mark(node.span());
        }
        visit::visit_expr_match(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let redundant = node.args.len() == 1
            && node.args.first().is_some_and(|argument| match argument {
                Expr::Closure(closure) if closure.inputs.len() == 1 => {
                    match closure.inputs.first() {
                        Some(Pat::Ident(input)) => {
                            matches!(closure.body.as_ref(), Expr::Path(path) if path.qself.is_none() && path.path.is_ident(&input.ident))
                                && node.method == "map"
                        }
                        Some(Pat::Wild(_)) => {
                            bool_expr(&closure.body) == Some(true) && node.method == "filter"
                        }
                        _ => false,
                    }
                }
                _ => false,
            });
        if redundant {
            self.mark(node.span());
        }
        visit::visit_expr_method_call(self, node);
    }
}

fn bool_block(block: &syn::Block) -> Option<bool> {
    match block.stmts.as_slice() {
        [Stmt::Expr(expression, None)] => bool_expr(expression),
        _ => None,
    }
}

fn bool_expr(expression: &Expr) -> Option<bool> {
    match expression {
        Expr::Lit(ExprLit {
            lit: Lit::Bool(value),
            ..
        }) => Some(value.value),
        Expr::Block(ExprBlock { block, .. }) => bool_block(block),
        Expr::Paren(parenthesized) => bool_expr(&parenthesized.expr),
        _ => None,
    }
}

fn count_lines(code_lines: &[bool], span: Span) -> usize {
    let start = span.start().line.max(1);
    let end = span.end().line.max(start).min(code_lines.len());
    (start..=end)
        .filter(|line| code_lines.get(line - 1).copied().unwrap_or(false))
        .count()
}

fn strip_rust_comments(source: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        Block(usize),
        String,
        Raw(usize),
    }

    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut state = State::Code;
    while index < bytes.len() {
        match state {
            State::Code if bytes[index..].starts_with(b"//") => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    output.push(b' ');
                    index += 1;
                }
            }
            State::Code if bytes[index..].starts_with(b"/*") => {
                output.extend_from_slice(b"  ");
                index += 2;
                state = State::Block(1);
            }
            State::Code if bytes[index] == b'"' => {
                output.push(bytes[index]);
                index += 1;
                state = State::String;
            }
            State::Code => {
                if let Some((hashes, consumed)) = raw_string_start(&bytes[index..]) {
                    output.extend_from_slice(&bytes[index..index + consumed]);
                    index += consumed;
                    state = State::Raw(hashes);
                } else {
                    output.push(bytes[index]);
                    index += 1;
                }
            }
            State::Block(depth) if bytes[index..].starts_with(b"/*") => {
                output.extend_from_slice(b"  ");
                index += 2;
                state = State::Block(depth + 1);
            }
            State::Block(depth) if bytes[index..].starts_with(b"*/") => {
                output.extend_from_slice(b"  ");
                index += 2;
                state = if depth == 1 {
                    State::Code
                } else {
                    State::Block(depth - 1)
                };
            }
            State::Block(_) => {
                output.push(if bytes[index] == b'\n' { b'\n' } else { b' ' });
                index += 1;
            }
            State::String if bytes[index] == b'\\' && index + 1 < bytes.len() => {
                output.extend_from_slice(&bytes[index..index + 2]);
                index += 2;
            }
            State::String => {
                output.push(bytes[index]);
                if bytes[index] == b'"' {
                    state = State::Code;
                }
                index += 1;
            }
            State::Raw(hashes) => {
                if raw_string_end(&bytes[index..], hashes) {
                    let consumed = hashes + 1;
                    output.extend_from_slice(&bytes[index..index + consumed]);
                    index += consumed;
                    state = State::Code;
                } else {
                    output.push(bytes[index]);
                    index += 1;
                }
            }
        }
    }
    String::from_utf8(output).expect("comment removal preserves UTF-8 bytes")
}

fn raw_string_start(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut index = match bytes.first() {
        Some(b'r') => 1,
        Some(b'b') if bytes.get(1) == Some(&b'r') => 2,
        _ => return None,
    };
    let start = index;
    while bytes.get(index) == Some(&b'#') {
        index += 1;
    }
    (bytes.get(index) == Some(&b'"')).then_some((index - start, index + 1))
}

fn raw_string_end(bytes: &[u8], hashes: usize) -> bool {
    bytes.first() == Some(&b'"')
        && bytes
            .get(1..1 + hashes)
            .is_some_and(|suffix| suffix.len() == hashes && suffix.iter().all(|byte| *byte == b'#'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, content: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn measures_verbosity_union_and_erosion_formula() {
        let root = tempfile::tempdir().unwrap();
        let duplicated = r#"
fn copied(value: Result<u64, String>) -> bool {
    let normalized = value.map(|number| number + 10);
    let bounded = normalized.map(|number| number.min(100));
    let rendered = bounded.map(|number| number.to_string());
    if rendered.is_ok() { true } else { false }
}
"#;
        write(root.path(), "a.rs", duplicated);
        write(root.path(), "b.rs", duplicated);
        write(
            root.path(),
            "complex.rs",
            r#"
fn tangled(a: bool, b: bool, c: bool, d: bool, e: bool, f: bool) -> bool {
    if a { return true; }
    if b { return true; }
    if c { return true; }
    if d { return true; }
    if e { return true; }
    if f { return true; }
    if a && b && c && d && e { return true; }
    false
}
fn redundant(value: bool) -> bool {
    if value { true } else { false }
}
"#,
        );

        let report = analyze(root.path()).unwrap();
        assert_eq!(report.files, 3);
        assert!(report.verbosity.clone_lines >= 8);
        assert!(report.verbosity.ast_flagged_lines >= 1);
        assert!(
            report.verbosity.flagged_or_clone_lines
                < report.verbosity.ast_flagged_lines + report.verbosity.clone_lines
        );
        assert!(report.verbosity.flagged_or_clone_lines <= report.source_lines);
        assert_eq!(report.erosion.complex_functions, 1);
        let expected = report.erosion.complex_mass / report.erosion.total_mass;
        assert!((report.erosion.ratio - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn erosion_uses_complexity_weighted_sqrt_sloc_and_a_strict_threshold() {
        let metric = |name: &str, complexity, source_lines| FunctionMetric {
            path: "fixture.rs".into(),
            name: name.into(),
            start_line: 1,
            source_lines,
            cyclomatic_complexity: complexity,
            mass: complexity as f64 * (source_lines as f64).sqrt(),
        };
        let report = erosion_report(
            vec![
                metric("ordinary", 5, 16),
                metric("threshold", 10, 4),
                metric("complex", 11, 9),
            ],
            20,
        );

        assert_eq!(report.functions, 3);
        assert_eq!(report.complex_functions, 1);
        assert_eq!(report.complex_mass, 33.0);
        assert_eq!(report.total_mass, 73.0);
        assert!((report.ratio - 33.0 / 73.0).abs() < f64::EPSILON);
    }

    #[test]
    fn ignores_dependency_roots_and_compares_reports() {
        let baseline = tempfile::tempdir().unwrap();
        let current = tempfile::tempdir().unwrap();
        write(baseline.path(), "src/lib.rs", "fn small() {}\n");
        write(
            current.path(),
            "src/lib.rs",
            "fn small() {}\nfn added() {}\n",
        );
        write(current.path(), "target/generated.rs", "fn ignored() {}\n");
        write(
            current.path(),
            "vendor/dependency.rs",
            "fn ignored_too() {}\n",
        );

        let assessment = assess(current.path(), Some(baseline.path())).unwrap();
        assert_eq!(assessment.current.files, 1);
        assert_eq!(assessment.delta.unwrap().source_lines, 1);
    }

    #[test]
    fn analyzes_mixed_and_unknown_source_languages() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "app.py",
            "# heading\ndef ready(value):\n    return value\n",
        );
        write(
            root.path(),
            "client.ts",
            "// heading\nexport const ready = (value: boolean) => value;\n",
        );
        write(
            root.path(),
            "module.futurelang",
            "-- syntax is intentionally unknown\nconstruct ready value\n",
        );
        write(root.path(), "README.md", "not source\n");
        fs::write(root.path().join("image.custom"), [0, 1, 2, 3]).unwrap();

        let report = analyze(root.path()).unwrap();

        assert_eq!(report.version, 2);
        assert_eq!(report.language, "mixed");
        assert_eq!(report.files, 3);
        assert_eq!(report.languages.get("python"), Some(&1));
        assert_eq!(report.languages.get("typescript"), Some(&1));
        assert_eq!(report.languages.get("futurelang"), Some(&1));
        assert_eq!(report.source_lines, 5);
        assert_eq!(report.erosion.functions, 0);
        assert_eq!(report.limitations.len(), 1);
    }

    #[test]
    fn clone_detection_is_language_independent() {
        let root = tempfile::tempdir().unwrap();
        let duplicated = concat!(
            "prepare shared application state\n",
            "validate shared application state\n",
            "transform shared application state\n",
            "publish shared application state\n",
        );
        write(root.path(), "first.future", duplicated);
        write(root.path(), "second.another", duplicated);

        let report = analyze(root.path()).unwrap();

        assert_eq!(report.languages.len(), 2);
        assert_eq!(report.verbosity.clone_lines, 8);
        assert_eq!(report.verbosity.flagged_or_clone_lines, 8);
    }

    #[cfg(unix)]
    #[test]
    fn skips_ambient_special_files() {
        use std::os::unix::net::UnixListener;

        let root = tempfile::tempdir().unwrap();
        write(root.path(), "code.rs", "fn included() {}\n");
        fs::create_dir(root.path().join(".ao")).unwrap();
        let _listener = UnixListener::bind(root.path().join(".ao/browser.sock")).unwrap();

        let report = analyze(root.path()).unwrap();

        assert_eq!(report.files, 1);
        assert_eq!(report.source_lines, 1);
    }

    #[test]
    fn accepts_inputs_beyond_the_previous_file_and_byte_limits_in_parallel_batches() {
        use std::io::Write as _;

        const PREVIOUS_MAX_FILES: usize = 20_000;
        const PREVIOUS_MAX_BYTES: usize = 64 * 1024 * 1024;

        let root = tempfile::tempdir().unwrap();
        for index in 0..=PREVIOUS_MAX_FILES {
            write(root.path(), &format!("files/{index:05}.future"), "x\n");
        }
        let large_path = root.path().join("large.future");
        let mut large = fs::File::create(&large_path).unwrap();
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..64 {
            large.write_all(&chunk).unwrap();
        }
        large.write_all(b"x").unwrap();
        drop(large);

        let files = PREVIOUS_MAX_FILES + 2;
        let batch_size = analysis_batch_size(files, 4);
        assert!(files.div_ceil(batch_size) > 1);
        let report = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap()
            .install(|| analyze(root.path()))
            .unwrap();

        assert_eq!(report.files, PREVIOUS_MAX_FILES + 2);
        assert!(report.bytes > PREVIOUS_MAX_BYTES as u64);
        assert_eq!(report.source_lines, PREVIOUS_MAX_FILES + 2);
    }

    #[test]
    fn comments_and_comment_markers_in_strings_are_handled() {
        let cleaned = strip_rust_comments(
            "// heading\nfn value() { let a = \"// text\"; /* nested /* x */ y */ let b = r#\"/* raw */\"#; }\n",
        );
        let lines = cleaned.lines().map(normalize_line).collect::<Vec<_>>();
        assert!(lines[0].is_empty());
        assert!(lines[1].contains("// text"));
        assert!(lines[1].contains("/* raw */"));
        assert!(!lines[1].contains("nested"));
    }
}
