use super::*;

fn syms(path: &str, src: &str) -> Vec<(String, &'static str, u32, Option<String>)> {
    let lang = Lang::detect(path.as_bytes(), src.as_bytes()).expect("lang");
    extract(lang, src.as_bytes())
        .into_iter()
        .map(|s| (s.name, s.kind.name(), s.line, s.container))
        .collect()
}

fn has(v: &[(String, &'static str, u32, Option<String>)], name: &str, kind: &str, line: u32, container: Option<&str>) {
    assert!(
        v.iter().any(|(n, k, l, c)| n == name && *k == kind && *l == line && c.as_deref() == container),
        "missing {name} {kind} {line} {container:?} in {v:#?}"
    );
}

#[test]
fn c() {
    let v = syms(
        "a.c",
        "#define MAX(a, b) ((a) > (b) ? (a) : (b))\n\
         struct point { int x; };\n\
         typedef struct point point_t;\n\
         enum color { RED, GREEN };\n\
         static int helper(int x);\n\
         static int helper(int x) { return x + 1; }\n\
         char *name_of(struct point *p) {\n\
           helper(1);\n\
           p->ops->free(p);\n\
           return 0;\n\
         }\n",
    );
    has(&v, "MAX", "macro", 1, None);
    has(&v, "point", "struct", 2, None);
    has(&v, "point_t", "type", 3, None);
    has(&v, "color", "enum", 4, None);
    has(&v, "RED", "constant", 4, None);
    has(&v, "helper", "prototype", 5, None);
    has(&v, "helper", "function", 6, None);
    has(&v, "name_of", "function", 7, None);
    has(&v, "helper", "call", 8, Some("name_of"));
    has(&v, "free", "call", 9, Some("name_of"));
}

#[test]
fn cpp() {
    let v = syms(
        "a.cc",
        "namespace ns {\n\
         class Widget {\n\
          public:\n\
           void Draw() const;\n\
           int size() { return Count(); }\n\
         };\n\
         void Widget::Draw() const {\n\
           auto w = std::make_unique<Widget>();\n\
           w->Paint(1);\n\
           ns::Log(\"x\");\n\
           Widget other;\n\
         }\n\
         Widget::~Widget() {}\n\
         template <typename T> T Max(T a, T b) { return a; }\n\
         using Id = int;\n\
         }\n",
    );
    has(&v, "ns", "module", 1, None);
    has(&v, "Widget", "class", 2, None);
    has(&v, "Draw", "method", 4, None);
    has(&v, "size", "method", 5, None);
    has(&v, "Count", "call", 5, Some("size"));
    has(&v, "Draw", "method", 7, None);
    has(&v, "make_unique", "call", 8, Some("Draw"));
    has(&v, "Paint", "call", 9, Some("Draw"));
    has(&v, "Log", "call", 10, Some("Draw"));
    has(&v, "Widget", "method", 13, None);
    has(&v, "Max", "function", 14, None);
    has(&v, "Id", "type", 15, None);
}

#[test]
fn header_detection() {
    assert_eq!(Lang::detect(b"x/a.h", b"struct s { int new; };"), Some(Lang::C));
    assert_eq!(Lang::detect(b"x/a.h", b"namespace a { }"), Some(Lang::Cpp));
    assert_eq!(Lang::detect(b"x/a.h", b"struct pid_namespace *ns;\nasm(\"\" ::: \"memory\");"), Some(Lang::C));
    assert_eq!(Lang::detect(b"x/a.h", b"#include <vector>\n"), Some(Lang::Cpp));
    assert_eq!(Lang::detect(b"pkg/BUILD.bazel", b""), Some(Lang::Starlark));
    assert_eq!(Lang::detect(b"README", b""), None);
}

#[test]
fn rust() {
    let v = syms(
        "a.rs",
        "struct S;\n\
         impl S {\n\
             fn new() -> S { helper(); S }\n\
         }\n\
         trait T { fn go(&self); }\n\
         fn helper() { let s = S::new(); s.run(); println!(\"x\"); }\n\
         const MAX: u32 = 3;\n",
    );
    has(&v, "S", "struct", 1, None);
    has(&v, "new", "method", 3, None);
    has(&v, "helper", "call", 3, Some("new"));
    has(&v, "T", "trait", 5, None);
    has(&v, "go", "method", 5, None);
    has(&v, "helper", "function", 6, None);
    has(&v, "new", "call", 6, Some("helper"));
    has(&v, "run", "call", 6, Some("helper"));
    has(&v, "println", "call", 6, Some("helper"));
    has(&v, "MAX", "constant", 7, None);
}

#[test]
fn go() {
    let v = syms(
        "a.go",
        "package main\n\
         type Server struct{}\n\
         type Handler interface { Serve() }\n\
         func (s *Server) Start() { s.listen(); fmt.Println(\"x\") }\n\
         func main() { NewServer().Start() }\n\
         const Port = 80\n",
    );
    has(&v, "Server", "struct", 2, None);
    has(&v, "Handler", "interface", 3, None);
    has(&v, "Serve", "method", 3, None);
    has(&v, "Start", "method", 4, None);
    has(&v, "listen", "call", 4, Some("Start"));
    has(&v, "Println", "call", 4, Some("Start"));
    has(&v, "NewServer", "call", 5, Some("main"));
    has(&v, "Start", "call", 5, Some("main"));
    has(&v, "Port", "constant", 6, None);
}

#[test]
fn python() {
    let v = syms(
        "a.py",
        "MAX = 3\n\
         lower = 1\n\
         class A:\n\
         \x20   @staticmethod\n\
         \x20   def go(self):\n\
         \x20       helper(1)\n\
         \x20       self.run()\n\
         def helper(x):\n\
         \x20   def inner():\n\
         \x20       pass\n\
         \x20   return inner()\n",
    );
    has(&v, "MAX", "constant", 1, None);
    assert!(!v.iter().any(|s| s.0 == "lower"));
    has(&v, "A", "class", 3, None);
    has(&v, "go", "method", 5, None);
    has(&v, "helper", "call", 6, Some("go"));
    has(&v, "run", "call", 7, Some("go"));
    has(&v, "helper", "function", 8, None);
    has(&v, "inner", "function", 9, None);
    has(&v, "inner", "call", 11, Some("helper"));
}

#[test]
fn starlark() {
    let v = syms(
        "pkg/BUILD",
        "load(\"//tools:defs.bzl\", \"my_rule\")\n\
         cc_library(\n\
         \x20   name = \"core\",\n\
         \x20   srcs = [\"a.cc\"],\n\
         )\n",
    );
    has(&v, "core", "target", 3, None);
    has(&v, "cc_library", "call", 2, None);
}

#[test]
fn lua() {
    let v = syms(
        "a.lua",
        "local M = {}\n\
         function M.setup(opts) M.start() end\n\
         function M:stop() vim.notify('x') end\n\
         local function helper() end\n\
         M.run = function() helper() end\n",
    );
    has(&v, "setup", "function", 2, None);
    has(&v, "start", "call", 2, Some("setup"));
    has(&v, "stop", "method", 3, None);
    has(&v, "notify", "call", 3, Some("stop"));
    has(&v, "helper", "function", 4, None);
    has(&v, "run", "function", 5, None);
    has(&v, "helper", "call", 5, Some("run"));
}

#[test]
fn javascript_and_typescript() {
    let v = syms(
        "a.js",
        "class A { go() { helper(); this.run(); } }\n\
         function helper() { return new A(); }\n\
         const f = () => helper();\n",
    );
    has(&v, "A", "class", 1, None);
    has(&v, "go", "method", 1, None);
    has(&v, "helper", "call", 1, Some("go"));
    has(&v, "run", "call", 1, Some("go"));
    has(&v, "helper", "function", 2, None);
    has(&v, "A", "call", 2, Some("helper"));
    has(&v, "f", "function", 3, None);
    has(&v, "helper", "call", 3, Some("f"));

    let v = syms(
        "a.ts",
        "interface Shape { area(): number }\n\
         type Id = string;\n\
         enum Color { Red }\n\
         export class Sq implements Shape { area() { return calc(1); } }\n\
         function calc(n: number): number { return n; }\n",
    );
    has(&v, "Shape", "interface", 1, None);
    has(&v, "area", "method", 1, None);
    has(&v, "Id", "type", 2, None);
    has(&v, "Color", "enum", 3, None);
    has(&v, "Sq", "class", 4, None);
    has(&v, "calc", "call", 4, Some("area"));
    has(&v, "calc", "function", 5, None);

    let v = syms("a.tsx", "function App() { return <div>{render()}</div>; }\n");
    has(&v, "App", "function", 1, None);
    has(&v, "render", "call", 1, Some("App"));
}
