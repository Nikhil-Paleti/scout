//! A tiny argument parser: no dependencies, long options only plus a few
//! short aliases.

use anyhow::{bail, Result};

pub struct Args {
    items: Vec<String>,
}

impl Args {
    pub fn new(items: Vec<String>) -> Args {
        Args { items }
    }

    /// Removes `--name` (or an alias) and returns whether it was present.
    /// Stops at `--`, so `scout -- -l` searches for `-l`.
    pub fn flag(&mut self, names: &[&str]) -> bool {
        let mut found = false;
        let mut rest = false;
        self.items.retain(|a| {
            if a == "--" {
                rest = true;
            }
            if !rest && names.contains(&a.as_str()) {
                found = true;
                false
            } else {
                true
            }
        });
        found
    }

    /// Removes `--name value` or `--name=value`; the last one wins.
    pub fn opt(&mut self, names: &[&str]) -> Result<Option<String>> {
        Ok(self.opts(names)?.pop())
    }

    /// Removes every `--name value` / `--name=value`.
    pub fn opts(&mut self, names: &[&str]) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.items.len() {
            if self.items[i] == "--" {
                break;
            }
            let a = &self.items[i];
            if names.contains(&a.as_str()) {
                if i + 1 >= self.items.len() {
                    bail!("{} needs a value", a);
                }
                out.push(self.items.remove(i + 1));
                self.items.remove(i);
                continue;
            }
            if let Some((k, v)) = a.split_once('=') {
                if names.contains(&k) {
                    out.push(v.to_string());
                    self.items.remove(i);
                    continue;
                }
            }
            i += 1;
        }
        Ok(out)
    }

    pub fn parsed<T: std::str::FromStr>(&mut self, names: &[&str]) -> Result<Option<T>> {
        match self.opt(names)? {
            None => Ok(None),
            Some(v) => match v.parse() {
                Ok(x) => Ok(Some(x)),
                Err(_) => bail!("bad value `{}` for {}", v, names[0]),
            },
        }
    }

    /// Remaining query terms, for commands that take a query. Known flags
    /// are already removed, so a leftover `-file:test` or `-foo` is an
    /// exclusion term, as in Google Code Search. Unknown `--long` options
    /// still error, since they are almost always typos.
    pub fn terms(self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut rest = false;
        for a in self.items {
            if rest {
                out.push(a);
            } else if a == "--" {
                rest = true;
            } else if a.starts_with("--") {
                bail!(
                    "unknown option {} (use -- before a query that starts with --)",
                    a
                );
            } else {
                out.push(a);
            }
        }
        Ok(out)
    }

    /// Remaining positional arguments; errors on unknown options.
    pub fn positionals(self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut rest = false;
        for a in self.items {
            if rest {
                out.push(a);
            } else if a == "--" {
                rest = true;
            } else if a.starts_with("--") || (a.starts_with('-') && a.len() > 1 && a != "-") {
                bail!("unknown option {}", a);
            } else {
                out.push(a);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Args {
        Args::new(v.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn dash_terms_are_query_terms() {
        let mut a = args(&["launch", "-file:test", "-lang:go", "-foo", "-l"]);
        assert!(a.flag(&["--files", "-l"]));
        assert_eq!(
            a.terms().unwrap(),
            vec!["launch", "-file:test", "-lang:go", "-foo"]
        );
    }

    #[test]
    fn known_flags_still_flags() {
        let mut a = args(&["-m", "5", "-i", "x", "-y"]);
        assert_eq!(a.parsed::<usize>(&["--max", "-m"]).unwrap(), Some(5));
        assert!(a.flag(&["-i"]));
        assert_eq!(a.terms().unwrap(), vec!["x", "-y"]);
    }

    #[test]
    fn unknown_long_option_errors() {
        assert!(args(&["x", "--nope"]).terms().is_err());
        assert_eq!(args(&["--", "--nope"]).terms().unwrap(), vec!["--nope"]);
    }

    #[test]
    fn flags_stop_at_double_dash() {
        let mut a = args(&["--", "-l"]);
        assert!(!a.flag(&["-l"]));
        assert_eq!(a.terms().unwrap(), vec!["-l"]);
    }
}
