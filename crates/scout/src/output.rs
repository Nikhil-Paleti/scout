//! Printing search hits: ripgrep-style text, vimgrep lines, JSON lines for
//! agents and editors, or file names only.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use scout_query::{Hit, SearchStats};
use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// `path:line:col:text`, colored and annotated on a terminal.
    Text,
    /// `path:line:col:text`, never colored or annotated.
    Vimgrep,
    /// One JSON object per line, then a summary object.
    Json,
    /// Matching file names, once each.
    Files,
}

pub struct Printer<W: Write> {
    out: W,
    format: Format,
    color: bool,
    before: usize,
    after: usize,
    root: PathBuf,
    last_file: String,
    /// Lines of the file the last hit was in, for context.
    cache: Option<(String, Vec<Vec<u8>>)>,
    /// Last line printed (hit or context) in `last_file`.
    last_printed: u64,
    printed_any: bool,
    pub hits: usize,
}

const PATH: &str = "\x1b[35m";
const LINE: &str = "\x1b[32m";
const MATCH: &str = "\x1b[1;31m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub fn stdout_is_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(1) == 1 }
}

impl<W: Write> Printer<W> {
    pub fn new(out: W, format: Format, color: bool, before: usize, after: usize, root: &Path) -> Printer<W> {
        Printer {
            out,
            format,
            color: color && format == Format::Text,
            before,
            after,
            root: root.to_path_buf(),
            last_file: String::new(),
            cache: None,
            last_printed: 0,
            printed_any: false,
            hits: 0,
        }
    }

    fn lines_of(&mut self, path: &str) -> &[Vec<u8>] {
        if self.cache.as_ref().map(|c| c.0 != path).unwrap_or(true) {
            let data = fs::read(self.root.join(path)).unwrap_or_default();
            let mut lines: Vec<Vec<u8>> = data.split(|&b| b == b'\n').map(|l| l.to_vec()).collect();
            if data.ends_with(b"\n") {
                lines.pop();
            }
            for l in lines.iter_mut() {
                if l.last() == Some(&b'\r') {
                    l.pop();
                }
            }
            self.cache = Some((path.to_string(), lines));
        }
        &self.cache.as_ref().unwrap().1
    }

    fn context(&mut self, path: &str, from: u64, to: u64) -> Vec<(u64, Vec<u8>)> {
        if from > to || from == 0 {
            return Vec::new();
        }
        let lines = self.lines_of(path);
        (from..=to).filter_map(|n| lines.get(n as usize - 1).map(|l| (n, l.clone()))).collect()
    }

    pub fn hit(&mut self, h: &Hit) -> io::Result<()> {
        self.hits += 1;
        match self.format {
            Format::Files => {
                if h.path != self.last_file {
                    self.last_file = h.path.clone();
                    writeln!(self.out, "{}", h.path)?;
                }
                Ok(())
            }
            Format::Json if h.is_file() => {
                let v = json!({
                    "type": "hit",
                    "kind": "file",
                    "path": h.path,
                    "line": 1,
                    "col": 1,
                    "text": "",
                    "ranges": h.ranges,
                });
                writeln!(self.out, "{}", v)
            }
            Format::Text if h.is_file() => {
                self.flush_after(None)?;
                if self.printed_any && self.before + self.after > 0 {
                    writeln!(self.out, "--")?;
                }
                self.last_file.clear();
                self.printed_any = true;
                if self.color {
                    let p = h.path.as_bytes();
                    let mut at = 0;
                    self.out.write_all(PATH.as_bytes())?;
                    for &(s, e) in &h.ranges {
                        if s < at || e > p.len() {
                            continue;
                        }
                        self.out.write_all(&p[at..s])?;
                        write!(self.out, "{MATCH}")?;
                        self.out.write_all(&p[s..e])?;
                        write!(self.out, "{RESET}{PATH}")?;
                        at = e;
                    }
                    self.out.write_all(&p[at..])?;
                    writeln!(self.out, "{RESET}  {DIM}[file]{RESET}")
                } else {
                    writeln!(self.out, "{}  [file]", h.path)
                }
            }
            Format::Json => {
                let mut v = json!({
                    "type": "hit",
                    "path": h.path,
                    "line": h.line,
                    "col": h.col,
                    "text": String::from_utf8_lossy(&h.text),
                    "kind": h.kind.unwrap_or("text"),
                    "ranges": h.ranges,
                });
                if let Some(s) = &h.symbol {
                    v["symbol"] = json!(s);
                }
                if let Some(c) = &h.container {
                    v["in"] = json!(c);
                }
                if self.before + self.after > 0 {
                    let b = self.context(&h.path, h.line.saturating_sub(self.before as u64).max(1), h.line - 1);
                    let a = self.context(&h.path, h.line + 1, h.line + self.after as u64);
                    let text = |v: Vec<(u64, Vec<u8>)>| {
                        v.into_iter().map(|(_, l)| String::from_utf8_lossy(&l).into_owned()).collect::<Vec<_>>()
                    };
                    v["before"] = json!(text(b));
                    v["after"] = json!(text(a));
                }
                writeln!(self.out, "{}", v)
            }
            Format::Text | Format::Vimgrep => {
                self.flush_after(Some(h))?;
                let same_file = h.path == self.last_file;
                if self.before + self.after > 0 {
                    let start = h.line.saturating_sub(self.before as u64).max(1);
                    let start = if same_file { start.max(self.last_printed + 1) } else { start };
                    if self.printed_any && (!same_file || start > self.last_printed + 1) {
                        writeln!(self.out, "--")?;
                    }
                    for (n, l) in self.context(&h.path, start, h.line.saturating_sub(1)) {
                        self.line(&h.path, n, None, &l, '-')?;
                    }
                }
                self.last_file = h.path.clone();
                self.line(&h.path, h.line, Some(h), &h.text, ':')?;
                self.last_printed = h.line;
                self.printed_any = true;
                Ok(())
            }
        }
    }

    /// After the last hit of a file, print its trailing context. Called with
    /// the next hit (or at the end) so overlapping context isn't repeated.
    pub fn flush_after(&mut self, next: Option<&Hit>) -> io::Result<()> {
        if self.after == 0 || self.last_file.is_empty() || !matches!(self.format, Format::Text | Format::Vimgrep) {
            return Ok(());
        }
        let path = self.last_file.clone();
        let mut end = self.last_printed + self.after as u64;
        if let Some(n) = next {
            if n.path == path {
                end = end.min(n.line.saturating_sub(1));
            }
        }
        for (n, l) in self.context(&path, self.last_printed + 1, end) {
            self.line(&path, n, None, &l, '-')?;
            self.last_printed = n;
        }
        Ok(())
    }

    fn line(&mut self, path: &str, n: u64, h: Option<&Hit>, text: &[u8], sep: char) -> io::Result<()> {
        let c = self.color;
        let w = &mut self.out;
        if c {
            write!(w, "{PATH}{path}{RESET}{sep}{LINE}{n}{RESET}{sep}")?;
        } else {
            write!(w, "{path}{sep}{n}{sep}")?;
        }
        if let Some(h) = h {
            write!(w, "{}:", h.col)?;
        }
        match h {
            Some(h) if c && !h.ranges.is_empty() => {
                let mut at = 0;
                for &(s, e) in &h.ranges {
                    if s < at || e > text.len() {
                        continue;
                    }
                    w.write_all(&text[at..s])?;
                    w.write_all(MATCH.as_bytes())?;
                    w.write_all(&text[s..e])?;
                    w.write_all(RESET.as_bytes())?;
                    at = e;
                }
                w.write_all(&text[at..])?;
            }
            _ => w.write_all(text)?,
        }
        if let (Some(h), Format::Text) = (h, self.format) {
            if let Some(k) = h.kind {
                let note = match &h.container {
                    Some(f) if k == "call" => format!("  [call in {}]", f),
                    _ => format!("  [{}]", k),
                };
                if c {
                    write!(w, "{DIM}{note}{RESET}")?;
                } else {
                    write!(w, "{note}")?;
                }
            }
        }
        w.write_all(b"\n")
    }

    /// `building` marks results that came from a scan because the index
    /// doesn't exist yet.
    pub fn summary(&mut self, stats: &SearchStats, building: bool) -> io::Result<()> {
        if self.format == Format::Json {
            let mut v = json!({
                "type": "summary",
                "matches": stats.matches,
                "files": stats.files_matched,
                "file_hits": stats.file_hits,
                "truncated": stats.truncated,
                "elapsed_ms": stats.total_time.as_secs_f64() * 1000.0,
            });
            if stats.symbols_incomplete {
                v["symbols_incomplete"] = json!(true);
            }
            v["index_status"] = json!(if building { "building" } else { "ready" });
            writeln!(self.out, "{}", v)?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}
