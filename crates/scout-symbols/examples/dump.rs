//! Prints the symbols of one file: `dump PATH [NAME]`.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let data = std::fs::read(&a[0]).unwrap();
    let lang = scout_symbols::Lang::detect(a[0].as_bytes(), &data);
    eprintln!("lang {:?}", lang.map(|l| l.name()));
    let t = std::time::Instant::now();
    let syms = scout_symbols::extract_path(a[0].as_bytes(), &data);
    eprintln!("{} syms in {:.1?}", syms.len(), t.elapsed());
    for s in syms {
        if a.get(1).map(|n| *n == s.name).unwrap_or(true) {
            println!("{}:{} {} {} {:?}", s.line, s.col, s.kind.name(), s.name, s.container);
        }
    }
}
