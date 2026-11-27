//! Which paths get indexed.
//!
//! Every file under the root is a candidate, tracked or untracked, hidden or
//! not. Excluded are `.git` internals, anything `.gitignore` (or
//! `.git/info/exclude`, or the global gitignore) ignores, and the
//! per-machine `exclude` globs. A `.scoutignore` file (gitignore syntax) has
//! higher precedence than `.gitignore`, so its `!` lines re-include paths
//! that git ignores.
//!
//! The parallel walker (`ignore::WalkBuilder`) is used for full builds and
//! [`Rules::check`] answers the same question for a single path, mirroring
//! the walker's precedence, for incremental updates and `status --why`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::overrides::{Override, OverrideBuilder};
use ignore::{Match, WalkBuilder};

pub const SCOUT_IGNORE: &str = ".scoutignore";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Included,
    Excluded(String),
}

impl Verdict {
    pub fn is_included(&self) -> bool {
        matches!(self, Verdict::Included)
    }
}

#[derive(Default)]
struct DirRules {
    scout: Option<Gitignore>,
    git: Option<Gitignore>,
    git_exclude: Option<Gitignore>,
    has_git: bool,
}

pub struct Rules {
    root: PathBuf,
    excludes: Vec<String>,
    overrides: Override,
    global: Gitignore,
    cache: Mutex<HashMap<PathBuf, Arc<DirRules>>>,
}

fn load_ignore(dir: &Path, file: &Path) -> Option<Gitignore> {
    if !file.is_file() {
        return None;
    }
    let mut b = GitignoreBuilder::new(dir);
    b.add(file);
    b.build().ok().filter(|g| !g.is_empty())
}

fn glob_reason(g: &ignore::gitignore::Glob, root: &Path) -> String {
    let from = g
        .from()
        .map(|p| p.strip_prefix(root).unwrap_or(p).display().to_string())
        .unwrap_or_else(|| "global gitignore".to_string());
    format!("{} rule `{}`", from, g.original())
}

impl Rules {
    pub fn new(root: &Path, excludes: &[String]) -> Result<Rules> {
        let mut ob = OverrideBuilder::new(root);
        for g in excludes {
            let g = g.trim_end_matches('/');
            ob.add(&format!("!{}", g))?;
            // `foo/**` should also prune the directory itself.
            if let Some(dir) = g.strip_suffix("/**") {
                ob.add(&format!("!{}", dir))?;
            }
        }
        let overrides = ob.build()?;
        let (global, _) = Gitignore::global();
        Ok(Rules {
            root: root.to_path_buf(),
            excludes: excludes.to_vec(),
            overrides,
            global,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn excludes(&self) -> &[String] {
        &self.excludes
    }

    /// A walker configured with the same rules as [`Rules::check`].
    pub fn walker(&self, threads: usize) -> WalkBuilder {
        let mut b = WalkBuilder::new(&self.root);
        b.hidden(false)
            .ignore(false)
            .parents(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .require_git(false)
            .follow_links(false)
            .add_custom_ignore_filename(SCOUT_IGNORE)
            .overrides(self.overrides.clone())
            .threads(threads)
            .filter_entry(|e| e.file_name() != ".git");
        b
    }

    fn dir_rules(&self, dir: &Path) -> Arc<DirRules> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(r) = cache.get(dir) {
            return r.clone();
        }
        let git_dir = dir.join(".git");
        let has_git = git_dir.exists();
        let r = Arc::new(DirRules {
            scout: load_ignore(dir, &dir.join(SCOUT_IGNORE)),
            git: load_ignore(dir, &dir.join(".gitignore")),
            git_exclude: if git_dir.is_dir() {
                load_ignore(dir, &git_dir.join("info").join("exclude"))
            } else {
                None
            },
            has_git,
        });
        cache.insert(dir.to_path_buf(), r.clone());
        r
    }

    /// Forgets cached ignore files (call after an ignore file changes).
    pub fn reload(&self) {
        self.cache.lock().unwrap().clear();
    }

    /// Decides one entry, assuming its parent directories are included.
    fn check_entry(&self, abs: &Path, is_dir: bool) -> Verdict {
        match self.overrides.matched(abs, is_dir) {
            Match::Ignore(_) => {
                let which = self
                    .excludes
                    .iter()
                    .find(|g| {
                        Rules::new(&self.root, std::slice::from_ref(g))
                            .map(|r| r.overrides.matched(abs, is_dir).is_ignore())
                            .unwrap_or(false)
                    })
                    .cloned()
                    .unwrap_or_default();
                return Verdict::Excluded(format!("exclude option `{}`", which));
            }
            Match::Whitelist(_) => return Verdict::Included,
            Match::None => {}
        }
        let mut m_scout: Match<ignore::gitignore::Glob> = Match::None;
        let mut m_git = Match::None;
        let mut m_excl = Match::None;
        let mut saw_git = false;
        let mut dir = abs.parent();
        while let Some(d) = dir {
            if !d.starts_with(&self.root) {
                break;
            }
            let r = self.dir_rules(d);
            if m_scout.is_none() {
                if let Some(g) = &r.scout {
                    m_scout = g.matched(abs, is_dir).map(|g| g.clone());
                }
            }
            if !saw_git && m_git.is_none() {
                if let Some(g) = &r.git {
                    m_git = g.matched(abs, is_dir).map(|g| g.clone());
                }
            }
            if !saw_git && m_excl.is_none() {
                if let Some(g) = &r.git_exclude {
                    m_excl = g.matched(abs, is_dir).map(|g| g.clone());
                }
            }
            saw_git = saw_git || r.has_git;
            if d == self.root {
                break;
            }
            dir = d.parent();
        }
        let mut m = m_scout.or(m_git).or(m_excl);
        if m.is_none() {
            m = self.global.matched(abs, is_dir).map(|g| g.clone());
        }
        match m {
            Match::Ignore(g) => Verdict::Excluded(glob_reason(&g, &self.root)),
            _ => Verdict::Included,
        }
    }

    /// Decides whether `rel` (relative to the root, `/`-separated) would be
    /// indexed by the walker, ignoring content-based rules.
    pub fn check(&self, rel: &Path, is_dir: bool) -> Verdict {
        let comps: Vec<_> = rel.components().collect();
        if comps.iter().any(|c| c.as_os_str() == ".git") {
            return Verdict::Excluded(".git internals".to_string());
        }
        let mut cur = self.root.clone();
        for (i, c) in comps.iter().enumerate() {
            cur.push(c);
            let last = i + 1 == comps.len();
            let v = self.check_entry(&cur, if last { is_dir } else { true });
            if let Verdict::Excluded(why) = v {
                if last {
                    return Verdict::Excluded(why);
                }
                let dir_rel = cur
                    .strip_prefix(&self.root)
                    .unwrap_or(&cur)
                    .display()
                    .to_string();
                return Verdict::Excluded(format!("directory {}/ is excluded by {}", dir_rel, why));
            }
        }
        Verdict::Included
    }

    /// True if `rel` names an ignore file whose change alters the indexed set.
    pub fn is_ignore_file(rel: &Path) -> bool {
        matches!(
            rel.file_name().and_then(|n| n.to_str()),
            Some(".gitignore") | Some(SCOUT_IGNORE)
        ) || rel.ends_with(".git/info/exclude")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;

    fn touch(root: &Path, rel: &str, data: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }

    fn walk(rules: &Rules) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for e in rules.walker(1).build() {
            let e = e.unwrap();
            if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                out.insert(
                    e.path()
                        .strip_prefix(rules.root())
                        .unwrap()
                        .display()
                        .to_string(),
                );
            }
        }
        out
    }

    #[test]
    fn walker_and_check_agree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir_all(root.join(".git/info")).unwrap();
        touch(&root, ".git/config", "x");
        touch(&root, ".git/info/exclude", "secret.txt\n");
        touch(&root, ".gitignore", "build/\n*.o\ngen/\n");
        touch(&root, ".scoutignore", "!gen/\ndocs/private.md\n");
        touch(&root, ".github/workflows/ci.yml", "on: push");
        touch(&root, ".clang-format", "x");
        touch(&root, "src/main.rs", "fn main() {}");
        touch(&root, "src/main.o", "obj");
        touch(&root, "src/.gitignore", "local.rs\n");
        touch(&root, "src/local.rs", "x");
        touch(&root, "build/out.rs", "x");
        touch(&root, "gen/api.h", "x");
        touch(&root, "docs/private.md", "x");
        touch(&root, "docs/public.md", "x");
        touch(&root, "secret.txt", "x");
        touch(&root, "third_party/lib/a.c", "x");

        let rules = Rules::new(&root, &["third_party/**".to_string()]).unwrap();
        let walked = walk(&rules);
        let expected: BTreeSet<String> = [
            ".clang-format",
            ".github/workflows/ci.yml",
            ".gitignore",
            ".scoutignore",
            "docs/public.md",
            "gen/api.h",
            "src/.gitignore",
            "src/main.rs",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(walked, expected);

        let all = [
            ".git/config",
            ".clang-format",
            ".github/workflows/ci.yml",
            "src/main.rs",
            "src/main.o",
            "src/local.rs",
            "build/out.rs",
            "gen/api.h",
            "docs/private.md",
            "docs/public.md",
            "secret.txt",
            "third_party/lib/a.c",
        ];
        for rel in all {
            let included = rules.check(Path::new(rel), false).is_included();
            assert_eq!(
                included,
                expected.contains(rel),
                "check disagrees with walker on {}",
                rel
            );
        }
        match rules.check(Path::new("build/out.rs"), false) {
            Verdict::Excluded(why) => assert!(why.contains("build/"), "{}", why),
            v => panic!("{:?}", v),
        }
        match rules.check(Path::new("third_party/lib/a.c"), false) {
            Verdict::Excluded(why) => assert!(why.contains("exclude option"), "{}", why),
            v => panic!("{:?}", v),
        }
    }
}
