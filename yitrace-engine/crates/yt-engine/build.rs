// 把构建好的控制台前端（console_dist/）在编译期嵌进二进制 —— 零外部依赖，气隙可部署。
//
// 流程：在 yitrace-console 里 `vite build`，把 dist/ 拷到本 crate 的 console_dist/，再 cargo build。
// console_dist/ 不存在时生成空表（引擎照常编译，静态路由返回 404），所以前端没构建也不挡引擎。
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=console_dist");
    let out = env::var("OUT_DIR").unwrap();
    let mut entries: Vec<(String, &'static str, String)> = Vec::new();
    let root = Path::new("console_dist");
    if root.exists() {
        walk(root, root, &mut entries);
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut code = String::from("// 自动生成，勿改。\npub static ASSETS: &[(&str, &str, &[u8])] = &[\n");
    for (url, ct, relpath) in &entries {
        code.push_str(&format!(
            "    ({url:?}, {ct:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/\", {relpath:?}))),\n"
        ));
    }
    code.push_str("];\n");
    fs::write(Path::new(&out).join("assets.rs"), code).unwrap();
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, &'static str, String)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(root, &p, out);
        } else {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let url = format!("/{rel}");
            let relpath = p.to_string_lossy().replace('\\', "/");
            out.push((url, content_type(&p), relpath));
        }
    }
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        _ => "application/octet-stream",
    }
}
