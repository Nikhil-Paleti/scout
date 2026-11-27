//! Times parsing vs. querying over files listed on stdin.
use std::io::BufRead;
use std::time::Instant;

fn main() {
    let mut bytes = 0usize;
    let mut n = 0usize;
    let mut syms = 0usize;
    let t = Instant::now();
    let mut parse = std::time::Duration::ZERO;
    for line in std::io::stdin().lock().lines() {
        let p = line.unwrap();
        let data = std::fs::read(&p).unwrap();
        let Some(lang) = scout_symbols::Lang::detect(p.as_bytes(), &data) else {
            continue;
        };
        bytes += data.len();
        n += 1;
        let t1 = Instant::now();
        scout_symbols::parse_only(lang, &data);
        parse += t1.elapsed();
        syms += scout_symbols::extract(lang, &data).len();
    }
    let total = t.elapsed();
    eprintln!(
        "{n} files, {:.1} MB, {syms} syms; parse {:.2?} ({:.1} MB/s); parse+query {:.2?}",
        bytes as f64 / 1e6,
        parse,
        bytes as f64 / 1e6 / parse.as_secs_f64(),
        total - parse
    );
}
