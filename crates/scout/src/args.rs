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
    pub fn flag(&mut self, names: &[&str]) -> bool {
        let mut found = false;
        self.items.retain(|a| {
            if names.contains(&a.as_str()) {
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
